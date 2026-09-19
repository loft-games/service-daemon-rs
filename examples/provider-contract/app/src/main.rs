//! Runnable app for the provider-contract example.
//!
//! Run with `cargo run -p example-provider-contract`.

use example_provider_contract_shared as shared;
use service_daemon::{
    ProviderError, Registry, RestartPolicy, ServiceDaemon, ServiceError, provider_impl,
};
use shared::SharedSettings;
use tracing::{error, info};

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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    service_daemon::init_logging();

    let daemon = ServiceDaemon::builder()
        .with_registry(
            Registry::builder()
                .with_tag("provider-contract-example")
                .build(),
        )
        .with_restart_policy(RestartPolicy::for_testing())
        .build();
    let cancel = daemon.cancel_token();

    daemon.run().await;
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    cancel.cancel();

    match daemon.wait().await {
        Ok(()) => {
            info!("Provider contract example daemon stopped cleanly");
            Ok(())
        }
        Err(ServiceError::InternalError(message)) => {
            error!(
                reason = %message,
                "Provider contract example could not install an OS shutdown signal listener"
            );
            Err(ServiceError::InternalError(message).into())
        }
        Err(error) => {
            error!(
                %error,
                "Provider contract example daemon wait failed outside the documented signal-listener path"
            );
            Err(error.into())
        }
    }
}
