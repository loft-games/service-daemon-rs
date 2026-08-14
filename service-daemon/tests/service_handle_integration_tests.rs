use service_daemon::{
    ManagedProvided, ProviderError, Registry, RestartPolicy, ServiceDaemon, ServiceHandle,
    ServiceStatus, done, provider, service, service_handle, sleep, wait_shutdown,
};
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::Duration;
use tokio::sync::Mutex as AsyncMutex;

static HANDLE_CONSUMER_READY: AtomicBool = AtomicBool::new(false);
static ON_DEMAND_HANDLE_READY: AtomicBool = AtomicBool::new(false);
static ON_DEMAND_WORKER_STARTS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);
static ON_DEMAND_WORKER_HANDLE: Mutex<Option<ServiceHandle>> = Mutex::new(None);
static ON_DEMAND_INTERNAL_PROGRESS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);
static ON_DEMAND_WORKER_INPUT_SUM: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);
static LIFECYCLE_HANDLES_READY: AtomicBool = AtomicBool::new(false);
static NORMAL_RESTART_HANDLE: Mutex<Option<ServiceHandle>> = Mutex::new(None);
static RECOVERABLE_RESTART_HANDLE: Mutex<Option<ServiceHandle>> = Mutex::new(None);
static PANIC_RESTART_HANDLE: Mutex<Option<ServiceHandle>> = Mutex::new(None);
static RELOAD_RESTART_HANDLE: Mutex<Option<ServiceHandle>> = Mutex::new(None);
static ISOLATION_HANDLE: Mutex<Option<ServiceHandle>> = Mutex::new(None);
static NORMAL_INPUT_GENERATIONS: AtomicUsize = AtomicUsize::new(0);
static RECOVERABLE_INPUT_GENERATIONS: AtomicUsize = AtomicUsize::new(0);
static PANIC_INPUT_GENERATIONS: AtomicUsize = AtomicUsize::new(0);
static RELOAD_INPUT_GENERATIONS: AtomicUsize = AtomicUsize::new(0);
static ISOLATION_INPUT_GENERATIONS: AtomicUsize = AtomicUsize::new(0);
static NORMAL_INPUT_DROPS: AtomicUsize = AtomicUsize::new(0);
static NORMAL_INPUT_PTRS: LazyLock<AsyncMutex<Vec<usize>>> =
    LazyLock::new(|| AsyncMutex::new(Vec::new()));
static RECOVERABLE_INPUT_PTRS: LazyLock<AsyncMutex<Vec<usize>>> =
    LazyLock::new(|| AsyncMutex::new(Vec::new()));
static PANIC_INPUT_PTRS: LazyLock<AsyncMutex<Vec<usize>>> =
    LazyLock::new(|| AsyncMutex::new(Vec::new()));
static RELOAD_INPUT_PTRS: LazyLock<AsyncMutex<Vec<usize>>> =
    LazyLock::new(|| AsyncMutex::new(Vec::new()));
static ISOLATION_INPUT_VALUES: LazyLock<AsyncMutex<Vec<(usize, usize)>>> =
    LazyLock::new(|| AsyncMutex::new(Vec::new()));

#[derive(Clone)]
struct SelectedWorkerHandle(ServiceHandle);

#[allow(dead_code)]
#[derive(Clone)]
struct ExcludedWorkerHandle(ServiceHandle);

#[derive(Clone)]
struct OnDemandWorkerHandle(ServiceHandle);

#[derive(Clone)]
struct InternalWorkerHandle(ServiceHandle);

#[derive(Clone)]
struct NormalRestartWorkerHandle(ServiceHandle);

#[derive(Clone)]
struct RecoverableRestartWorkerHandle(ServiceHandle);

#[derive(Clone)]
struct PanicRestartWorkerHandle(ServiceHandle);

#[derive(Clone)]
struct ReloadRestartWorkerHandle(ServiceHandle);

#[derive(Clone)]
struct IsolationWorkerHandle(ServiceHandle);

struct WorkerJob {
    value: usize,
}

struct LifecycleInput {
    value: usize,
    drop_counter: Option<&'static AtomicUsize>,
}

impl Drop for LifecycleInput {
    fn drop(&mut self) {
        if let Some(counter) = self.drop_counter {
            counter.fetch_add(1, Ordering::SeqCst);
        }
    }
}

#[derive(Clone, Default)]
#[provider]
struct LifecycleReloadConfig {
    version: usize,
}

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

#[service(tags = ["__service_handle_on_demand_spawn__"])]
async fn on_demand_worker(#[input] job: &WorkerJob) -> anyhow::Result<()> {
    ON_DEMAND_WORKER_STARTS.fetch_add(1, Ordering::SeqCst);
    ON_DEMAND_WORKER_INPUT_SUM.fetch_add(job.value, Ordering::SeqCst);
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

#[service(tags = ["__service_handle_internal_on_demand_spawn__"])]
async fn internal_on_demand_worker(#[input] job: &WorkerJob) -> anyhow::Result<()> {
    ON_DEMAND_INTERNAL_PROGRESS.fetch_add(job.value, Ordering::SeqCst);
    done();
    wait_shutdown().await;
    Ok(())
}

#[provider]
fn internal_worker_handle() -> Result<InternalWorkerHandle, ProviderError> {
    service_handle!(internal_on_demand_worker).map(InternalWorkerHandle)
}

#[service(tags = ["__service_handle_internal_on_demand_spawn__"])]
async fn internal_on_demand_controller(
    handle: std::sync::Arc<InternalWorkerHandle>,
) -> anyhow::Result<()> {
    let service = handle.0.clone();
    done();
    tokio::spawn(async move {
        let instance = service
            .start(WorkerJob { value: 10 })
            .await
            .expect("started service should create and start on-demand worker after startup waves");
        while ON_DEMAND_INTERNAL_PROGRESS.load(Ordering::SeqCst) < 11 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        instance
            .remove()
            .await
            .expect("started service should remove on-demand worker");
        ON_DEMAND_INTERNAL_PROGRESS.store(12, Ordering::SeqCst);
    });
    wait_shutdown().await;
    Ok(())
}

#[service(tags = ["__service_input_lifecycle__"])]
async fn normal_restart_worker(#[input] input: &LifecycleInput) -> anyhow::Result<()> {
    let generation = NORMAL_INPUT_GENERATIONS.fetch_add(1, Ordering::SeqCst) + 1;
    NORMAL_INPUT_PTRS
        .lock()
        .await
        .push(input as *const LifecycleInput as usize);
    assert_eq!(input.value, 11);
    done();
    if generation == 1 {
        return Ok(());
    }
    wait_shutdown().await;
    Ok(())
}

#[provider]
fn normal_restart_worker_handle() -> Result<NormalRestartWorkerHandle, ProviderError> {
    service_handle!(normal_restart_worker).map(NormalRestartWorkerHandle)
}

#[service(tags = ["__service_input_lifecycle__"])]
async fn recoverable_restart_worker(#[input] input: &LifecycleInput) -> anyhow::Result<()> {
    let generation = RECOVERABLE_INPUT_GENERATIONS.fetch_add(1, Ordering::SeqCst) + 1;
    RECOVERABLE_INPUT_PTRS
        .lock()
        .await
        .push(input as *const LifecycleInput as usize);
    assert_eq!(input.value, 22);
    done();
    if generation == 1 {
        return Err(anyhow::anyhow!("recoverable input restart"));
    }
    wait_shutdown().await;
    Ok(())
}

#[provider]
fn recoverable_restart_worker_handle() -> Result<RecoverableRestartWorkerHandle, ProviderError> {
    service_handle!(recoverable_restart_worker).map(RecoverableRestartWorkerHandle)
}

#[service(tags = ["__service_input_lifecycle__"])]
async fn panic_restart_worker(#[input] input: &LifecycleInput) -> anyhow::Result<()> {
    let generation = PANIC_INPUT_GENERATIONS.fetch_add(1, Ordering::SeqCst) + 1;
    PANIC_INPUT_PTRS
        .lock()
        .await
        .push(input as *const LifecycleInput as usize);
    assert_eq!(input.value, 33);
    done();
    assert!(generation != 1, "panic input restart");
    wait_shutdown().await;
    Ok(())
}

#[provider]
fn panic_restart_worker_handle() -> Result<PanicRestartWorkerHandle, ProviderError> {
    service_handle!(panic_restart_worker).map(PanicRestartWorkerHandle)
}

#[service(tags = ["__service_input_lifecycle__"])]
async fn reload_restart_worker(
    #[input] input: &LifecycleInput,
    _config: std::sync::Arc<RwLock<LifecycleReloadConfig>>,
) -> anyhow::Result<()> {
    RELOAD_INPUT_GENERATIONS.fetch_add(1, Ordering::SeqCst);
    RELOAD_INPUT_PTRS
        .lock()
        .await
        .push(input as *const LifecycleInput as usize);
    assert_eq!(input.value, 44);

    loop {
        match service_daemon::state() {
            ServiceStatus::Initializing | ServiceStatus::Restoring => done(),
            ServiceStatus::Healthy => {
                if !sleep(Duration::from_millis(10)).await {
                    continue;
                }
            }
            ServiceStatus::NeedReload => {
                done();
                break;
            }
            ServiceStatus::ShuttingDown | ServiceStatus::Terminated => break,
            ServiceStatus::Recovering(_) => break,
            _ => break,
        }
    }
    Ok(())
}

#[provider]
fn reload_restart_worker_handle() -> Result<ReloadRestartWorkerHandle, ProviderError> {
    service_handle!(reload_restart_worker).map(ReloadRestartWorkerHandle)
}

#[service(tags = ["__service_input_lifecycle__"])]
async fn isolation_worker(#[input] input: &LifecycleInput) -> anyhow::Result<()> {
    ISOLATION_INPUT_GENERATIONS.fetch_add(1, Ordering::SeqCst);
    ISOLATION_INPUT_VALUES
        .lock()
        .await
        .push((input.value, input as *const LifecycleInput as usize));
    done();
    wait_shutdown().await;
    Ok(())
}

#[provider]
fn isolation_worker_handle() -> Result<IsolationWorkerHandle, ProviderError> {
    service_handle!(isolation_worker).map(IsolationWorkerHandle)
}

#[service(tags = ["__service_input_lifecycle__"], priority = 80)]
async fn input_lifecycle_handle_catalog(
    normal: std::sync::Arc<NormalRestartWorkerHandle>,
    recoverable: std::sync::Arc<RecoverableRestartWorkerHandle>,
    panic_worker: std::sync::Arc<PanicRestartWorkerHandle>,
    reload: std::sync::Arc<ReloadRestartWorkerHandle>,
    isolation: std::sync::Arc<IsolationWorkerHandle>,
) -> anyhow::Result<()> {
    *NORMAL_RESTART_HANDLE
        .lock()
        .expect("normal restart handle mutex should not be poisoned") = Some(normal.0.clone());
    *RECOVERABLE_RESTART_HANDLE
        .lock()
        .expect("recoverable restart handle mutex should not be poisoned") =
        Some(recoverable.0.clone());
    *PANIC_RESTART_HANDLE
        .lock()
        .expect("panic restart handle mutex should not be poisoned") = Some(panic_worker.0.clone());
    *RELOAD_RESTART_HANDLE
        .lock()
        .expect("reload restart handle mutex should not be poisoned") = Some(reload.0.clone());
    *ISOLATION_HANDLE
        .lock()
        .expect("isolation handle mutex should not be poisoned") = Some(isolation.0.clone());
    LIFECYCLE_HANDLES_READY.store(true, Ordering::SeqCst);
    done();
    wait_shutdown().await;
    Ok(())
}

async fn wait_for_counter(counter: &AtomicUsize, expected: usize, reason: &'static str) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while counter.load(Ordering::SeqCst) < expected {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect(reason);
}

async fn wait_for_lifecycle_handles() {
    tokio::time::timeout(Duration::from_secs(2), async {
        while !LIFECYCLE_HANDLES_READY.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("lifecycle handle catalog should publish service handles");
}

fn clone_handle(slot: &Mutex<Option<ServiceHandle>>, name: &'static str) -> ServiceHandle {
    slot.lock()
        .expect("service handle mutex should not be poisoned")
        .clone()
        .unwrap_or_else(|| panic!("{name} service handle should be available"))
}

fn reset_handle(slot: &Mutex<Option<ServiceHandle>>) {
    slot.lock()
        .expect("service handle mutex should not be poisoned")
        .take();
}

fn assert_same_allocation(ptrs: &[usize], expected_len: usize, label: &'static str) {
    assert!(
        ptrs.len() >= expected_len,
        "{label} should record at least {expected_len} generations, got {ptrs:?}"
    );
    let first = ptrs[0];
    assert!(
        ptrs.iter().take(expected_len).all(|ptr| *ptr == first),
        "{label} should reuse the same input allocation across generations: {ptrs:?}"
    );
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
async fn input_service_instances_reuse_input_across_restart_reload_and_isolate_instances() {
    LIFECYCLE_HANDLES_READY.store(false, Ordering::SeqCst);
    NORMAL_INPUT_GENERATIONS.store(0, Ordering::SeqCst);
    RECOVERABLE_INPUT_GENERATIONS.store(0, Ordering::SeqCst);
    PANIC_INPUT_GENERATIONS.store(0, Ordering::SeqCst);
    RELOAD_INPUT_GENERATIONS.store(0, Ordering::SeqCst);
    ISOLATION_INPUT_GENERATIONS.store(0, Ordering::SeqCst);
    NORMAL_INPUT_DROPS.store(0, Ordering::SeqCst);
    NORMAL_INPUT_PTRS.lock().await.clear();
    RECOVERABLE_INPUT_PTRS.lock().await.clear();
    PANIC_INPUT_PTRS.lock().await.clear();
    RELOAD_INPUT_PTRS.lock().await.clear();
    ISOLATION_INPUT_VALUES.lock().await.clear();
    reset_handle(&NORMAL_RESTART_HANDLE);
    reset_handle(&RECOVERABLE_RESTART_HANDLE);
    reset_handle(&PANIC_RESTART_HANDLE);
    reset_handle(&RELOAD_RESTART_HANDLE);
    reset_handle(&ISOLATION_HANDLE);

    let registry = Registry::builder()
        .with_tag("__service_input_lifecycle__")
        .build();
    let daemon = ServiceDaemon::builder()
        .with_registry(registry)
        .with_restart_policy(RestartPolicy::for_testing())
        .build();

    daemon.run().await;
    wait_for_lifecycle_handles().await;

    let normal = clone_handle(&NORMAL_RESTART_HANDLE, "normal restart");
    let recoverable = clone_handle(&RECOVERABLE_RESTART_HANDLE, "recoverable restart");
    let panic_worker = clone_handle(&PANIC_RESTART_HANDLE, "panic restart");
    let reload = clone_handle(&RELOAD_RESTART_HANDLE, "reload restart");
    let isolation = clone_handle(&ISOLATION_HANDLE, "isolation");

    assert!(normal.instances().is_empty());
    assert!(recoverable.instances().is_empty());
    assert!(panic_worker.instances().is_empty());
    assert!(reload.instances().is_empty());
    assert!(isolation.instances().is_empty());

    let normal_instance = normal
        .start(LifecycleInput {
            value: 11,
            drop_counter: Some(&NORMAL_INPUT_DROPS),
        })
        .await
        .expect("normal restart input service should start");
    wait_for_counter(
        &NORMAL_INPUT_GENERATIONS,
        2,
        "normal exit should restart the input service",
    )
    .await;
    assert_same_allocation(&NORMAL_INPUT_PTRS.lock().await, 2, "normal restart");
    assert_eq!(
        NORMAL_INPUT_DROPS.load(Ordering::SeqCst),
        0,
        "stopping should not drop record-owned input before cleanup"
    );
    assert!(
        normal_instance
            .stop()
            .await
            .expect("normal restart instance should stop")
    );
    assert_eq!(
        NORMAL_INPUT_DROPS.load(Ordering::SeqCst),
        0,
        "stopped but registered instance should still own its input"
    );
    assert!(
        normal_instance
            .remove()
            .await
            .expect("normal restart instance should remove")
    );
    wait_for_counter(
        &NORMAL_INPUT_DROPS,
        1,
        "remove should release the record-owned input",
    )
    .await;

    let recoverable_instance = recoverable
        .start(LifecycleInput {
            value: 22,
            drop_counter: None,
        })
        .await
        .expect("recoverable input service should start");
    wait_for_counter(
        &RECOVERABLE_INPUT_GENERATIONS,
        2,
        "recoverable error should restart the input service",
    )
    .await;
    assert_same_allocation(
        &RECOVERABLE_INPUT_PTRS.lock().await,
        2,
        "recoverable restart",
    );
    assert!(
        recoverable_instance
            .remove()
            .await
            .expect("recoverable input instance should remove")
    );

    let panic_instance = panic_worker
        .start(LifecycleInput {
            value: 33,
            drop_counter: None,
        })
        .await
        .expect("panic input service should start");
    wait_for_counter(
        &PANIC_INPUT_GENERATIONS,
        2,
        "panic should restart the input service",
    )
    .await;
    assert_same_allocation(&PANIC_INPUT_PTRS.lock().await, 2, "panic restart");
    assert!(
        panic_instance
            .remove()
            .await
            .expect("panic input instance should remove")
    );

    let reload_instance = reload
        .start(LifecycleInput {
            value: 44,
            drop_counter: None,
        })
        .await
        .expect("reload input service should start");
    wait_for_counter(
        &RELOAD_INPUT_GENERATIONS,
        1,
        "reload input service should start first generation",
    )
    .await;
    {
        let config = <LifecycleReloadConfig as ManagedProvided>::resolve_rwlock()
            .await
            .expect("lifecycle reload config should resolve");
        let mut guard = config.write().await;
        guard.version += 1;
    }
    wait_for_counter(
        &RELOAD_INPUT_GENERATIONS,
        2,
        "dependency reload should restart the input service",
    )
    .await;
    assert_same_allocation(&RELOAD_INPUT_PTRS.lock().await, 2, "dependency reload");
    assert!(
        reload_instance
            .remove()
            .await
            .expect("reload input instance should remove")
    );

    let first = isolation
        .start(LifecycleInput {
            value: 101,
            drop_counter: None,
        })
        .await
        .expect("first isolated input instance should start");
    let second = isolation
        .start(LifecycleInput {
            value: 202,
            drop_counter: None,
        })
        .await
        .expect("second isolated input instance should start");
    wait_for_counter(
        &ISOLATION_INPUT_GENERATIONS,
        2,
        "multiple input instances should start independently",
    )
    .await;
    let isolation_records = ISOLATION_INPUT_VALUES.lock().await.clone();
    let values = isolation_records
        .iter()
        .map(|(value, _)| *value)
        .collect::<HashSet<_>>();
    assert_eq!(values, HashSet::from([101, 202]));
    let ptrs = isolation_records
        .iter()
        .map(|(_, ptr)| *ptr)
        .collect::<HashSet<_>>();
    assert_eq!(
        ptrs.len(),
        2,
        "different input instances should not share the same allocation: {isolation_records:?}"
    );
    assert!(
        first
            .remove()
            .await
            .expect("first isolated input instance should remove")
    );
    assert!(
        second
            .remove()
            .await
            .expect("second isolated input instance should remove")
    );

    daemon.shutdown();
    daemon
        .wait()
        .await
        .expect("daemon should shut down cleanly");
}

#[tokio::test]
async fn auto_start_service_rejects_non_unit_dynamic_input() {
    let registry = Registry::builder()
        .with_tag("__service_handle_success__")
        .build();
    let daemon = ServiceDaemon::builder().with_registry(registry).build();

    daemon.run().await;
    let service_handle = daemon
        .service_instances()
        .into_iter()
        .find(|instance| instance.name() == "selected_worker")
        .expect("selected worker should have one auto-start instance")
        .service();

    let err = service_handle
        .create("not unit")
        .await
        .expect_err("auto-start service should reject non-unit dynamic input");
    assert!(
        err.to_string()
            .contains("does not declare #[input] but received input type"),
        "unexpected non-unit input error: {err}"
    );

    daemon.shutdown();
    daemon
        .wait()
        .await
        .expect("daemon should shut down cleanly");
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
async fn on_demand_service_handle_creates_starts_stops_removes_and_force_removes_instances() {
    ON_DEMAND_HANDLE_READY.store(false, Ordering::SeqCst);
    ON_DEMAND_WORKER_STARTS.store(0, Ordering::SeqCst);
    ON_DEMAND_WORKER_INPUT_SUM.store(0, Ordering::SeqCst);
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
        "template services should not be auto-instantiated"
    );

    let wrong_input_error = on_demand_handle
        .create("wrong input")
        .await
        .expect_err("template service should reject mismatched input type");
    assert!(
        wrong_input_error
            .to_string()
            .contains("expects input 'job'"),
        "unexpected wrong-input error: {wrong_input_error}"
    );

    let first = on_demand_handle
        .create(WorkerJob { value: 2 })
        .await
        .expect("on-demand service should create a runtime instance");
    assert_eq!(on_demand_handle.instances().len(), 1);
    assert_eq!(
        first.status().await,
        service_daemon::ServiceStatus::Initializing
    );
    assert_eq!(
        ON_DEMAND_WORKER_STARTS.load(Ordering::SeqCst),
        0,
        "create should not start the worker"
    );
    assert!(first.start().await.expect("start should complete"));
    tokio::time::timeout(Duration::from_secs(2), async {
        while ON_DEMAND_WORKER_STARTS.load(Ordering::SeqCst) < 1 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("started on-demand worker should start");
    assert_eq!(first.name(), "on_demand_worker");
    assert_eq!(ON_DEMAND_WORKER_INPUT_SUM.load(Ordering::SeqCst), 2);
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
        .start(WorkerJob { value: 3 })
        .await
        .expect("on-demand service should create and start another runtime instance");
    tokio::time::timeout(Duration::from_secs(2), async {
        while ON_DEMAND_WORKER_STARTS.load(Ordering::SeqCst) < 2 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("second started on-demand worker should start");
    assert_eq!(ON_DEMAND_WORKER_INPUT_SUM.load(Ordering::SeqCst), 5);
    assert_eq!(on_demand_handle.instances().len(), 1);

    assert!(
        second
            .force_remove()
            .await
            .expect("force remove should complete")
    );
    assert!(second.runtime().is_none());
    assert!(on_demand_handle.instances().is_empty());

    daemon.shutdown();
    daemon
        .wait()
        .await
        .expect("daemon should shut down cleanly");
}

#[tokio::test]
async fn started_service_can_start_on_demand_instance_after_startup_waves() {
    ON_DEMAND_INTERNAL_PROGRESS.store(0, Ordering::SeqCst);
    let registry = Registry::builder()
        .with_tag("__service_handle_internal_on_demand_spawn__")
        .build();
    let daemon = ServiceDaemon::builder().with_registry(registry).build();

    daemon.run().await;
    tokio::time::timeout(Duration::from_secs(2), async {
        while ON_DEMAND_INTERNAL_PROGRESS.load(Ordering::SeqCst) < 2 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("started service should create, start, stop, and remove on-demand worker");

    daemon.shutdown();
    daemon
        .wait()
        .await
        .expect("daemon should shut down cleanly");
}
