use std::path::PathBuf;

mod env_provider;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    use clap::Parser;
    let cmd = VariablesCommand::parse();
    cmd.run().await
}

#[derive(clap::Parser)]
struct VariablesCommand {
    /// The application whose variables to view. This may be a manifest (spin.toml) file, a
    /// directory containing a spin.toml file, or a remote registry reference.
    /// If omitted, it defaults to "spin.toml".
    #[clap(short = 'f', long = "from")]
    from: Option<String>,

    /// Ignore server certificate errors from a registry
    #[clap(short = 'k', long = "insecure", num_args = 0)]
    insecure: bool,

    /// How to output the variables. The available options are:
    /// 
    /// * bash - a bash script which can be saved, edited, and used to export values
    /// * table - a human-readable tabular display format
    /// 
    /// The default is table.
    #[clap(short = 'o', long = "output", default_value = "table")]
    output: OutputFormat,
}

impl VariablesCommand {
    async fn run(&self) -> anyhow::Result<()> {
        let app_source = infer_app_source(&self.from)?;
        let app_manifest = app_source.resolve(self.insecure).await?;

        let variables = app_manifest.variables();

        if variables.is_empty() {
            println!("This application does not define any variables");
        } else {
            println!("{}", self.format_variables(&variables));

            if matches!(self.output, OutputFormat::Table) {
                let components = app_manifest.components();
                if !components.is_empty() {
                    match prepare_resolver(&variables).await {
                        Err(e) => anyhow::bail!("Unable to preview expansions: {e:#}"),
                        Ok(resolver) => println!("\n{}", self.expand_manifest_items(&resolver, &components).await),
                    }
                }
            }
        }

        Ok(())
    }

    fn format_variables(&self, variables: &[VariableInfo]) -> Box<dyn std::fmt::Display> {
        match self.output {
            OutputFormat::Table => Box::new(format_table(variables)),
            OutputFormat::Bash => Box::new(format_bash(variables)),
        }
    }

    async fn expand_manifest_items(&self, resolver: &spin_expressions::PreparedResolver, components: &[ComponentInfo]) -> impl std::fmt::Display {
        let mut table = comfy_table::Table::new();
        table.set_header(comfy_table::Row::from(vec!["Component", "Variable/Host", "Expanded value"]));
        table.load_preset(comfy_table::presets::ASCII_BORDERS_ONLY_CONDENSED);

        for component in components {
            let mut is_first_row = true;
            for (variable, template) in &component.variables {
                let expansion = expand_template_or_error(&resolver, template);
                table.add_row(vec![component.id_or_empty(is_first_row), variable.as_str(), expansion.as_str()]);
                is_first_row = false;
            }
            for host in &component.allowed_outbound_hosts {
                let expansion = expand_template_or_error(&resolver, host);
                table.add_row(vec![component.id_or_empty(is_first_row), host.as_str(), expansion.as_str()]);
                is_first_row = false;
            }
        }

        table
    }
}

async fn prepare_resolver(variables: &[VariableInfo]) -> anyhow::Result<spin_expressions::PreparedResolver> {
    let provider_vars = variables.iter().map(|v| (v.name.clone(), locked_variable(v)));
    let mut provider = spin_expressions::ProviderResolver::new(provider_vars)?;
    provider.add_provider(Box::new(env_provider::EnvProvider));
    let resolver = provider.prepare().await?;
    Ok(resolver)
}

fn locked_variable(variable: &VariableInfo) -> spin_locked_app::locked::Variable {
    spin_locked_app::Variable {
        default: variable.default_value.clone(),
        secret: variable.secret,
    }
}

fn expand_template_or_error(resolver: &spin_expressions::PreparedResolver, template: &str) -> String {
    expand_template(resolver, template).unwrap_or("Evalutation error!".to_string())
}

fn expand_template(resolver: &spin_expressions::PreparedResolver, template: &str) -> anyhow::Result<String> {
    let template = spin_expressions::Template::new(template)?;
    Ok(resolver.resolve_template(&template)?)
}

fn variables_from_toml(manifest: &spin_manifest::schema::v2::AppManifest) -> Vec<VariableInfo> {
    manifest.variables.clone().into_iter().map(|(name, variable)| VariableInfo {
        name: name.to_string(),
        default_value: variable.default,
        required: variable.required,
        secret: variable.secret,
    }).collect()
}

fn variables_from_registry_app(locked_app: &spin_locked_app::locked::LockedApp) -> Vec<VariableInfo> {
    locked_app.variables.clone().into_iter().map(|(name, variable)| VariableInfo {
        name,
        required: variable.default.is_none(),
        default_value: variable.default,
        secret: variable.secret,
    }).collect()
}

fn format_table(variables: &[VariableInfo]) -> impl std::fmt::Display {
    let mut table = comfy_table::Table::new();
    table.set_header(comfy_table::Row::from(vec!["Name", "Required?", "Default value", "Secret?"]));
    table.load_preset(comfy_table::presets::ASCII_BORDERS_ONLY_CONDENSED);

    for variable in variables {
        let default_value = variable.default_value.as_ref().map(|v| v.as_str()).unwrap_or_default();

        let required = if variable.required {
            "Required"
        } else {
            "Optional"
        };

        let secret = if variable.secret {
            "Secret"
        } else {
            ""
        };

        table.add_row(vec![
            variable.name.as_str(),
            required,
            default_value,
            secret,
        ]);
    }

    table
}

fn format_bash(variables: &[VariableInfo]) -> impl std::fmt::Display {
    let mut lines = vec![
        "# You may `source` this or reference it in your runtime-config.toml via the `dotenv_path` field".to_owned(),
        "".to_owned(),
    ];
    lines.extend(variables.iter().map(format_one_bash));
    lines.join("\n")
}

fn format_one_bash(variable: &VariableInfo) -> String {
    let env_var_name = format!("SPIN_VARIABLE_{}", variable.name.to_ascii_uppercase());
    match &variable.default_value {
        Some(default_value) => format!("# export {env_var_name}=\"{default_value}\"  # optional"),
        None => format!("export {env_var_name}=TO-DO  # required"),
    }
}

struct VariableInfo {
    name: String,
    default_value: Option<String>,
    required: bool,
    secret: bool,
}

struct ComponentInfo {
    id: String,
    allowed_outbound_hosts: Vec<String>,
    variables: Vec<(String, String)>,
}

impl ComponentInfo {
    fn has_expandables(&self) -> bool {
        !self.allowed_outbound_hosts.is_empty() || !self.variables.is_empty()
    }

    fn id_or_empty(&self, id_please: bool) -> &str {
        if id_please {
            &self.id
        } else {
            ""
        }
    }
}

enum AppSource {
    File(PathBuf),
    Registry(String),
}

enum AppManifest {
    User(spin_manifest::schema::v2::AppManifest),
    Locked(spin_locked_app::locked::LockedApp, #[allow(dead_code)] tempfile::TempDir), // it's not dead, it's just holding onto a Drop (although TODO: not sure if we need it to)
}

fn infer_app_source(provided: &Option<String>) -> anyhow::Result<AppSource> {
    match provided {
        None => Ok(AppSource::File(spin_common::paths::DEFAULT_MANIFEST_FILE.into())),
        Some(provided) if spin_oci::is_probably_oci_reference(provided) => Ok(AppSource::Registry(provided.clone())),
        Some(provided) => Ok(AppSource::File(spin_common::paths::resolve_manifest_file_path(provided)?)),
    }
}

impl AppSource {
    async fn resolve(&self, insecure: bool) -> anyhow::Result<AppManifest> {
        match self {
            AppSource::File(path) => Ok(AppManifest::User(spin_manifest::manifest_from_file(path)?)),
            AppSource::Registry(reference) => {
                let working_dir = tempfile::TempDir::with_prefix("spin-variables-")?;

                let mut client = spin_oci::Client::new(insecure, None).await?;
            
                let locked_app = spin_oci::OciLoader::new(working_dir.path())
                    .load_app(&mut client, reference)
                    .await?;

                Ok(AppManifest::Locked(locked_app, working_dir))
            },
        }
    }
}

impl AppManifest {
    fn variables(&self) -> Vec<VariableInfo> {
        match self {
            AppManifest::User(manifest) => variables_from_toml(&manifest),
            AppManifest::Locked(locked_app, _) => variables_from_registry_app(&locked_app),
        }        
    }

    fn components(&self) -> Vec<ComponentInfo> {
        match self {
            AppManifest::User(manifest) => components_from_toml(&manifest),
            AppManifest::Locked(locked_app, _) => components_from_registry_app(&locked_app),
        }        
    }
}

fn components_from_toml(manifest: &spin_manifest::schema::v2::AppManifest) -> Vec<ComponentInfo> {
    manifest.components.iter().map(|(id, component)| ComponentInfo {
        id: id.to_string(),
        allowed_outbound_hosts: component.allowed_outbound_hosts.clone().into_iter().filter(|h| h.contains("{{")).collect(),
        variables: component.variables.iter().map(|(name, template)| (name.to_string(), template.clone())).collect(),
    }).filter(|c| c.has_expandables()).collect()
}

fn components_from_registry_app(locked_app: &spin_locked_app::locked::LockedApp) -> Vec<ComponentInfo> {
    locked_app.components.iter().map(|component| ComponentInfo {
        id: component.id.clone(),
        allowed_outbound_hosts: component
            .metadata
            .get("allowed_outbound_hosts")
            .and_then(|v| v.as_array())
            .map(|v| v.clone())
            .unwrap_or_default()
            .iter()
            .filter_map(|v|
                v.as_str().map(|s| s.to_string())
            )
            .filter(|s| s.contains("{{"))
            .collect(),
        variables: component.config.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
    }).filter(|c| c.has_expandables()).collect()
}

#[derive(Clone, Debug, clap::ValueEnum)]
enum OutputFormat {
    Table,
    Bash,
}
