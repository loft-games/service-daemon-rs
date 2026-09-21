use example_provider_contract as _;
use example_provider_contract_shared::{self as shared, SharedSettings};
use service_daemon::{
    Provided, Registry, RestartPolicy, ServiceDaemon, done, service, wait_shutdown,
};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::time::timeout;

const RLIB_CANDIDATE_TAG: &str = "__provider_contract_rlib_candidate__";
static RESOLUTION: Mutex<Option<Result<String, String>>> = Mutex::new(None);

#[service(tags = ["__provider_contract_rlib_candidate__"])]
async fn library_candidate_consumer() -> anyhow::Result<()> {
    let resolution = <SharedSettings as Provided>::resolve()
        .await
        .map(|settings: Arc<SharedSettings>| settings.source().to_owned())
        .map_err(|error| error.to_string());
    *RESOLUTION
        .lock()
        .expect("resolution mutex should be usable") = Some(resolution);
    done();
    wait_shutdown().await;
    Ok(())
}

#[tokio::test]
async fn library_target_provider_impl_resolves_shared_contract() {
    *RESOLUTION
        .lock()
        .expect("resolution mutex should be usable") = None;
    let daemon = ServiceDaemon::builder()
        .with_registry(Registry::builder().with_tag(RLIB_CANDIDATE_TAG).build())
        .build();
    let cancel = daemon.cancel_token();

    daemon.run().await;
    let resolution = timeout(Duration::from_secs(5), async {
        loop {
            if let Some(resolution) = RESOLUTION
                .lock()
                .expect("resolution mutex should be usable")
                .clone()
            {
                return resolution;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("test service should publish its provider resolution result");

    cancel.cancel();
    timeout(Duration::from_secs(5), daemon.wait())
        .await
        .expect("example daemon shutdown should not time out")
        .expect("example daemon shutdown should succeed");

    assert_eq!(resolution, Ok("example fallback".to_owned()));
}

#[tokio::test]
async fn public_example_topology_uses_library_target_provider_impl() {
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
    .expect("shared service should run with the library-target provider implementation");

    cancel.cancel();
    timeout(Duration::from_secs(5), daemon.wait())
        .await
        .expect("example daemon shutdown should not time out")
        .expect("example daemon shutdown should succeed");

    assert!(shared::service_run_count() > 0);
}
