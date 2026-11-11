use service_daemon::{ProviderError, provider_contract, provider_impl, service};

#[derive(Clone)]
#[provider_contract]
pub struct SharedSettings {
    source: &'static str,
}

#[provider_impl(priority = 80)]
pub async fn primary_settings() -> Result<SharedSettings, ProviderError> {
    Err(ProviderError::Unavailable("not configured".to_owned()))
}

#[provider_impl(priority = 10)]
pub async fn fallback_settings() -> SharedSettings {
    SharedSettings { source: "fallback" }
}

#[service]
pub async fn uses_shared_settings(
    settings: std::sync::Arc<SharedSettings>,
) -> anyhow::Result<()> {
    let _source = settings.source;
    Ok(())
}

fn main() {}
