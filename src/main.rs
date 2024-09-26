use std::path::{Path, PathBuf};

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
    /// * kube - YAML containing secrets and configmaps for SpinKube
    /// * table - a human-readable tabular display format
    /// 
    /// The default is table.
    #[clap(short = 'o', long = "output", default_value = "table")]
    output: OutputFormat,
}

impl VariablesCommand {
    async fn run(&self) -> anyhow::Result<()> {
        let app_source = infer_app_source(&self.from)?;

        let variables = match app_source {
            AppSource::File(manifest_file) => variables_from_toml(&manifest_file).await?,
            AppSource::Registry(reference) => variables_from_registry_app(&reference, self.insecure).await?,
        };

        println!("{}", self.format_variables(&variables));

        Ok(())
    }

    fn format_variables(&self, variables: &[VariableInfo]) -> Box<dyn std::fmt::Display> {
        match self.output {
            OutputFormat::Table => Box::new(format_table(variables)),
            OutputFormat::Bash => Box::new(format_bash(variables)),
            OutputFormat::Kube => Box::new(format_kube(variables)),
        }
    }
}

async fn variables_from_toml(path: impl AsRef<Path>) -> anyhow::Result<Vec<VariableInfo>> {
    let manifest = spin_manifest::manifest_from_file(path)?;
    let variables = manifest.variables.into_iter().map(|(name, variable)| VariableInfo {
        name: name.to_string(),
        default_value: variable.default,
        required: variable.required,
        secret: variable.secret,
    }).collect();
    Ok(variables)
}

async fn variables_from_registry_app(reference: &str, insecure: bool) -> anyhow::Result<Vec<VariableInfo>> {
    let working_dir = tempfile::TempDir::with_prefix("spin-variables-")?;

    let mut client = spin_oci::Client::new(insecure, None).await?;

    let locked_app = spin_oci::OciLoader::new(working_dir.path())
        .load_app(&mut client, reference)
        .await?;

    let variables = locked_app.variables.into_iter().map(|(name, variable)| VariableInfo {
        name,
        required: variable.default.is_none(),
        default_value: variable.default,
        secret: variable.secret,
    }).collect();

    Ok(variables)
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

const KUBE_CONFIGMAP_NAME: &str = "spinapp-cfg";
const KUBE_SECRET_NAME: &str = "spinapp-secret";

fn format_kube(variables: &[VariableInfo]) -> impl std::fmt::Display {
    let (secrets, configs): (Vec<_>, Vec<_>) = variables.iter().partition(|v| v.secret);

    let mut blocks = vec![];

    if !configs.is_empty() {
        blocks.push(kube_block("ConfigMap", "v1", KUBE_CONFIGMAP_NAME, &configs));
    }
    if !secrets.is_empty() {
        blocks.push(kube_block("Secret", "v1", KUBE_SECRET_NAME, &secrets));
    }

    blocks.push(kube_app_block(variables));

    blocks.join("\n---\n")
}

fn kube_block(kind: &str, api_version: &str, name: &str, variables: &[&VariableInfo]) -> String {
    let mut lines = vec![
        format!("kind: {kind}"),
        format!("apiVersion: {api_version}"),
        format!("metadata:\n  name: {name}"),
        "data:".to_owned(),
    ];

    lines.extend(variables.iter().map(format_one_kube));
    lines.join("\n")
}

fn format_one_kube(variable: &&VariableInfo) -> String {
    let (prefix, default_value) = match &variable.default_value {
        Some(v) => ("# ", v.as_str()),
        None => ("", "TO-DO"),
    };
    format!("  {prefix}{}: {}", variable.kube_name(), default_value)
}

fn kube_app_block(variables: &[VariableInfo]) -> String {
    let mut lines = vec![
        "spec:".to_owned(),
        "  variables:".to_owned(),
    ];

    lines.extend(variables.iter().flat_map(format_one_for_app_block));
    lines.join("\n")
}

fn format_one_for_app_block(variable: &VariableInfo) -> Vec<String> {
    let (kind, store_name) = if variable.secret {
        ("secret", KUBE_SECRET_NAME)
    } else {
        ("configMap", KUBE_CONFIGMAP_NAME)
    };

    let mut lines = vec![];
    lines.push(format!("    - name: {}", variable.name));
    lines.push(format!("      valueFrom:"));
    lines.push(format!("        {}KeyRef:", kind));
    lines.push(format!("          name: {}", store_name));
    lines.push(format!("          key: {}", variable.kube_name()));
    lines.push(format!("          optional: {}", !variable.required));

    lines
}

struct VariableInfo {
    name: String,
    default_value: Option<String>,
    required: bool,
    secret: bool,
}

impl VariableInfo {
    fn kube_name(&self) -> String {
        use heck::ToLowerCamelCase;
        self.name.to_lower_camel_case()
    }
}

enum AppSource {
    File(PathBuf),
    Registry(String),
}

fn infer_app_source(provided: &Option<String>) -> anyhow::Result<AppSource> {
    match provided {
        None => Ok(AppSource::File(spin_common::paths::DEFAULT_MANIFEST_FILE.into())),
        Some(provided) if spin_oci::is_probably_oci_reference(provided) => Ok(AppSource::Registry(provided.clone())),
        Some(provided) => Ok(AppSource::File(spin_common::paths::resolve_manifest_file_path(provided)?)),
    }
}

#[derive(Clone, Debug, clap::ValueEnum)]
enum OutputFormat {
    Bash,
    Kube,
    Table,
}
