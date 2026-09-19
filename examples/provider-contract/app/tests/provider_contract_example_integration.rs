use example_provider_contract_shared as shared;
use service_daemon::{ProviderError, Registry, RestartPolicy, ServiceDaemon, provider_impl};
use shared::SharedSettings;
use std::time::Duration;
use tokio::time::timeout;

#[provider_impl(priority = 80)]
async fn test_primary_settings() -> Result<SharedSettings, ProviderError> {
    Err(ProviderError::Unavailable(
        "primary implementation absent in test".to_owned(),
    ))
}

#[provider_impl(priority = 10)]
async fn test_fallback_settings() -> SharedSettings {
    SharedSettings::new("test fallback")
}

#[tokio::test]
async fn app_local_provider_impl_resolves_shared_contract() {
    shared::reset_service_run_count();
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
    timeout(Duration::from_secs(5), async {
        while shared::service_run_count() == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("shared service should run with the app-local provider implementation");

    cancel.cancel();
    timeout(Duration::from_secs(5), daemon.wait())
        .await
        .expect("example daemon shutdown should not time out")
        .expect("example daemon shutdown should succeed");

    assert!(shared::service_run_count() > 0);
}
