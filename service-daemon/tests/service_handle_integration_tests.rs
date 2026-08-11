use service_daemon::{
    ProviderError, Registry, ServiceDaemon, ServiceHandle, done, provider, service, service_handle,
    wait_shutdown,
};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

static HANDLE_CONSUMER_READY: AtomicBool = AtomicBool::new(false);
static ON_DEMAND_HANDLE_READY: AtomicBool = AtomicBool::new(false);
static ON_DEMAND_WORKER_STARTS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);
static ON_DEMAND_WORKER_HANDLE: Mutex<Option<ServiceHandle>> = Mutex::new(None);

#[derive(Clone)]
struct SelectedWorkerHandle(ServiceHandle);

#[allow(dead_code)]
#[derive(Clone)]
struct ExcludedWorkerHandle(ServiceHandle);

#[derive(Clone)]
struct OnDemandWorkerHandle(ServiceHandle);

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
    assert!(
        handle
            .0
            .instances()
            .iter()
            .any(|instance| instance.name() == "selected_worker"),
        "service handle should list instances for the selected service in its daemon"
    );
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

#[service(auto_start = false, tags = ["__service_handle_on_demand_spawn__"])]
async fn on_demand_worker() -> anyhow::Result<()> {
    ON_DEMAND_WORKER_STARTS.fetch_add(1, Ordering::SeqCst);
    done();
    wait_shutdown().await;
    Ok(())
}

#[provider]
fn on_demand_worker_handle() -> Result<OnDemandWorkerHandle, ProviderError> {
    service_handle!(on_demand_worker).map(OnDemandWorkerHandle)
}

#[service(tags = ["__service_handle_on_demand_spawn__"])]
async fn on_demand_handle_consumer(
    handle: std::sync::Arc<OnDemandWorkerHandle>,
) -> anyhow::Result<()> {
    *ON_DEMAND_WORKER_HANDLE
        .lock()
        .expect("on-demand worker handle mutex should not be poisoned") = Some(handle.0.clone());
    ON_DEMAND_HANDLE_READY.store(true, Ordering::SeqCst);
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
    let daemon = ServiceDaemon::builder().with_registry(registry).build();

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
    let daemon = ServiceDaemon::builder().with_registry(registry).build();
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

#[tokio::test]
async fn on_demand_service_handle_spawns_stops_removes_and_purges_instances() {
    ON_DEMAND_HANDLE_READY.store(false, Ordering::SeqCst);
    ON_DEMAND_WORKER_STARTS.store(0, Ordering::SeqCst);
    ON_DEMAND_WORKER_HANDLE
        .lock()
        .expect("on-demand worker handle mutex should not be poisoned")
        .take();

    let registry = Registry::builder()
        .with_tag("__service_handle_on_demand_spawn__")
        .build();
    let daemon = ServiceDaemon::builder().with_registry(registry).build();

    daemon.run().await;
    tokio::time::timeout(Duration::from_secs(2), async {
        while !ON_DEMAND_HANDLE_READY.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("on-demand handle consumer should publish the service handle");

    let on_demand_handle = ON_DEMAND_WORKER_HANDLE
        .lock()
        .expect("on-demand worker handle mutex should not be poisoned")
        .clone()
        .expect("on-demand worker handle should be available");
    assert!(
        on_demand_handle.instances().is_empty(),
        "Non-auto-start services should not be auto-instantiated"
    );

    let first = on_demand_handle
        .spawn()
        .await
        .expect("on-demand service should spawn a runtime instance");
    tokio::time::timeout(Duration::from_secs(2), async {
        while ON_DEMAND_WORKER_STARTS.load(Ordering::SeqCst) < 1 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("spawned on-demand worker should start");
    assert_eq!(first.name(), "on_demand_worker");
    assert_eq!(on_demand_handle.instances().len(), 1);

    assert!(first.stop().await.expect("stop should complete"));
    assert_eq!(
        first.status().await,
        service_daemon::ServiceStatus::Terminated
    );
    assert!(
        first.runtime().is_some(),
        "stop should leave runtime facts registered until remove"
    );

    assert!(first.remove().await.expect("remove should complete"));
    assert!(first.runtime().is_none());
    assert!(on_demand_handle.instances().is_empty());

    let second = on_demand_handle
        .spawn()
        .await
        .expect("on-demand service should spawn another runtime instance");
    tokio::time::timeout(Duration::from_secs(2), async {
        while ON_DEMAND_WORKER_STARTS.load(Ordering::SeqCst) < 2 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("second spawned on-demand worker should start");
    assert_eq!(on_demand_handle.instances().len(), 1);

    assert!(second.purge().await.expect("purge should complete"));
    assert!(second.runtime().is_none());
    assert!(on_demand_handle.instances().is_empty());

    daemon.shutdown();
    daemon
        .wait()
        .await
        .expect("daemon should shut down cleanly");
}
