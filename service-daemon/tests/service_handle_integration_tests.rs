use service_daemon::{
    ProviderError, Registry, ServiceDaemon, ServiceHandle, done, provider, service, service_handle,
    wait_shutdown,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

static HANDLE_CONSUMER_READY: AtomicBool = AtomicBool::new(false);

#[derive(Clone)]
struct SelectedWorkerHandle(ServiceHandle);

#[allow(dead_code)]
#[derive(Clone)]
struct ExcludedWorkerHandle(ServiceHandle);

#[service(tags = ["__service_handle_success__"])]
async fn selected_worker() -> anyhow::Result<()> {
    done();
    wait_shutdown().await;
    Ok(())
}

#[provider]
fn selected_worker_handle() -> Result<SelectedWorkerHandle, ProviderError> {
    service_handle!(selected_worker).map(SelectedWorkerHandle)
}

#[service(tags = ["__service_handle_success__"])]
async fn selected_handle_consumer(
    handle: std::sync::Arc<SelectedWorkerHandle>,
) -> anyhow::Result<()> {
    assert_eq!(handle.0.name(), "selected_worker");
    HANDLE_CONSUMER_READY.store(true, Ordering::SeqCst);
    done();
    wait_shutdown().await;
    Ok(())
}

#[service(tags = ["__service_handle_excluded_target__"])]
async fn excluded_worker() -> anyhow::Result<()> {
    done();
    wait_shutdown().await;
    Ok(())
}

#[provider]
fn excluded_worker_handle() -> Result<ExcludedWorkerHandle, ProviderError> {
    service_handle!(excluded_worker).map(ExcludedWorkerHandle)
}

#[service(tags = ["__service_handle_excluded_consumer__"])]
async fn excluded_handle_consumer(
    _handle: std::sync::Arc<ExcludedWorkerHandle>,
) -> anyhow::Result<()> {
    done();
    wait_shutdown().await;
    Ok(())
}

#[tokio::test]
async fn provider_resolves_service_handle_in_selected_daemon_projection() {
    HANDLE_CONSUMER_READY.store(false, Ordering::SeqCst);
    let registry = Registry::builder()
        .with_tag("__service_handle_success__")
        .build();
    let mut daemon = ServiceDaemon::builder().with_registry(registry).build();

    daemon.run().await;
    let ready = tokio::time::timeout(Duration::from_secs(2), async {
        while !HANDLE_CONSUMER_READY.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;

    daemon.shutdown();
    daemon
        .wait()
        .await
        .expect("daemon should shut down cleanly");
    assert!(ready.is_ok(), "service handle consumer should become ready");
}

#[tokio::test]
async fn provider_reports_handle_target_outside_daemon_projection() {
    let registry = Registry::builder()
        .with_tag("__service_handle_excluded_consumer__")
        .build();
    let mut daemon = ServiceDaemon::builder().with_registry(registry).build();
    let token = daemon.cancel_token();

    daemon.run().await;
    let shutdown = tokio::time::timeout(Duration::from_secs(2), token.cancelled()).await;

    daemon.shutdown();
    daemon.wait().await.expect("daemon wait should complete");
    assert!(
        shutdown.is_ok(),
        "provider-init failure should request daemon shutdown"
    );
}
