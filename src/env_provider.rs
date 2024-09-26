#[derive(Debug)]
pub struct EnvProvider;

#[async_trait::async_trait]
impl spin_expressions::Provider for EnvProvider {
    async fn get(&self, key: &spin_expressions::Key) -> anyhow::Result<Option<String>> {
        let env_var_name = format!("SPIN_VARIABLE_{}", key.as_str().to_ascii_uppercase());
        Ok(std::env::var(&env_var_name).ok())
    }
}
