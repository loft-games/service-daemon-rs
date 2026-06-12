use service_daemon::provider;

#[derive(Clone, Default)]
pub struct FallibleConfig;

#[derive(Clone, Debug)]
pub struct ProviderError;

#[provider]
async fn fallible_config_provider() -> Result<FallibleConfig, ProviderError> {
    Ok(FallibleConfig)
}

fn main() {}
