//! Provider implementations compiled into the application library target.

use example_provider_contract_shared::SharedSettings;
use service_daemon::{ProviderError, provider_impl};

#[provider_impl(priority = 80)]
async fn primary_settings() -> Result<SharedSettings, ProviderError> {
    Err(ProviderError::Unavailable(
        "primary candidate is unavailable in this example".to_owned(),
    ))
}

#[provider_impl(priority = 10)]
async fn fallback_settings() -> SharedSettings {
    SharedSettings::new("example fallback")
}
