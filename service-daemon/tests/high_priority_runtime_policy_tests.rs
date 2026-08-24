use service_daemon::{
    DiagnosticHighPriorityPlacementDecisionKind, Registry, SchedulingAdvisoryProfile,
    ServiceDaemon, ServiceHandle, ServiceStatus, done, provider, service, service_handle,
    wait_shutdown,
};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

static HANDLE_READY: AtomicBool = AtomicBool::new(false);
static WORKER_HANDLE: LazyLock<Mutex<Option<ServiceHandle>>> = LazyLock::new(|| Mutex::new(None));
static WORKER_STARTS: AtomicUsize = AtomicUsize::new(0);
static TEST_LOCK: LazyLock<tokio::sync::Mutex<()>> = LazyLock::new(|| tokio::sync::Mutex::new(()));

struct PolicyWorkerJob {
    block_for: Duration,
}

#[derive(Clone)]
struct PolicyWorkerHandle(ServiceHandle);

#[service(
    tags = ["__high_priority_runtime_policy__"],
    scheduling = HighPriority
)]
async fn policy_worker(#[input] job: &PolicyWorkerJob) -> anyhow::Result<()> {
    WORKER_STARTS.fetch_add(1, Ordering::SeqCst);
    if !job.block_for.is_zero() {
        std::thread::sleep(job.block_for);
    }
    done();
    wait_shutdown().await;
    Ok(())
}

#[provider]
fn policy_worker_handle() -> Result<PolicyWorkerHandle, service_daemon::ProviderError> {
    service_handle!(policy_worker).map(PolicyWorkerHandle)
}

#[service(tags = ["__high_priority_runtime_policy__"])]
async fn policy_handle_consumer(handle: std::sync::Arc<PolicyWorkerHandle>) -> anyhow::Result<()> {
    *WORKER_HANDLE
        .lock()
        .unwrap_or_else(|err| panic!("worker handle lock poisoned: {err}")) =
        Some(handle.0.clone());
    HANDLE_READY.store(true, Ordering::SeqCst);
    done();
    wait_shutdown().await;
    Ok(())
}

fn reset_policy_test_state() {
    HANDLE_READY.store(false, Ordering::SeqCst);
    WORKER_STARTS.store(0, Ordering::SeqCst);
    *WORKER_HANDLE
        .lock()
        .unwrap_or_else(|err| panic!("worker handle lock poisoned: {err}")) = None;
}

fn policy_worker_service_handle() -> ServiceHandle {
    WORKER_HANDLE
        .lock()
        .unwrap_or_else(|err| panic!("worker handle lock poisoned: {err}"))
        .clone()
        .expect("policy worker handle should be ready")
}

async fn wait_until(mut predicate: impl FnMut() -> bool, label: &'static str, timeout: Duration) {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if predicate() {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {label}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn high_priority_policy_scales_out_and_places_new_dynamic_instance_on_new_shard() {
    let _guard = TEST_LOCK.lock().await;
    reset_policy_test_state();
    let daemon = ServiceDaemon::builder()
        .with_registry(
            Registry::builder()
                .with_tag("__high_priority_runtime_policy__")
                .build(),
        )
        .build();

    daemon.run().await;
    wait_until(
        || HANDLE_READY.load(Ordering::SeqCst),
        "policy worker handle",
        Duration::from_secs(2),
    )
    .await;
    assert_eq!(daemon.runtime().high_priority_shards.len(), 1);

    let handle = policy_worker_service_handle();
    let pressured = handle
        .start(PolicyWorkerJob {
            block_for: Duration::from_secs(3),
        })
        .await
        .expect("pressured worker should start");

    wait_until(
        || daemon.runtime().high_priority_shards.len() >= 2,
        "HighPriority policy scale-out",
        Duration::from_secs(5),
    )
    .await;

    let placed_after_scale_out = handle
        .start(PolicyWorkerJob {
            block_for: Duration::ZERO,
        })
        .await
        .expect("post-scale worker should start");
    wait_until(
        || {
            placed_after_scale_out
                .runtime()
                .is_some_and(|runtime| runtime.status == ServiceStatus::Healthy)
        },
        "post-scale worker readiness",
        Duration::from_secs(2),
    )
    .await;

    let pressured_runtime = pressured
        .runtime()
        .expect("pressured worker runtime should be visible");
    let placed_runtime = placed_after_scale_out
        .runtime()
        .expect("post-scale worker runtime should be visible");
    assert_ne!(
        pressured_runtime.high_priority_shard_id, placed_runtime.high_priority_shard_id,
        "new HighPriority generation should avoid the pressured shard after scale-out"
    );

    let diagnostics = daemon.diagnostics_snapshot();
    assert!(
        diagnostics
            .high_priority_placement_decisions
            .iter()
            .any(|decision| decision.kind == DiagnosticHighPriorityPlacementDecisionKind::ScaleOut),
        "diagnostics should retain the HighPriority scale-out decision"
    );
    assert!(
        diagnostics
            .services
            .iter()
            .any(|service| service.high_priority_shard_id == placed_runtime.high_priority_shard_id),
        "service diagnostics should include actual HighPriority shard placement"
    );

    daemon.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn high_priority_policy_scales_out_while_single_worker_shard_is_blocked() {
    let _guard = TEST_LOCK.lock().await;
    reset_policy_test_state();
    let daemon = ServiceDaemon::builder()
        .with_registry(
            Registry::builder()
                .with_tag("__high_priority_runtime_policy__")
                .build(),
        )
        .build();

    daemon.run().await;
    wait_until(
        || HANDLE_READY.load(Ordering::SeqCst),
        "policy worker handle",
        Duration::from_secs(2),
    )
    .await;

    policy_worker_service_handle()
        .start(PolicyWorkerJob {
            block_for: Duration::from_secs(3),
        })
        .await
        .expect("pressured worker should start");

    wait_until(
        || daemon.runtime().high_priority_shards.len() >= 2,
        "HighPriority scale-out while target shard is blocked",
        Duration::from_secs(5),
    )
    .await;

    daemon.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn advisory_disabled_does_not_disable_high_priority_policy() {
    let _guard = TEST_LOCK.lock().await;
    reset_policy_test_state();
    let daemon = ServiceDaemon::builder()
        .with_registry(
            Registry::builder()
                .with_tag("__high_priority_runtime_policy__")
                .build(),
        )
        .with_scheduling_advisory_profile(SchedulingAdvisoryProfile::disabled())
        .build();

    daemon.run().await;
    wait_until(
        || HANDLE_READY.load(Ordering::SeqCst),
        "policy worker handle",
        Duration::from_secs(2),
    )
    .await;

    policy_worker_service_handle()
        .start(PolicyWorkerJob {
            block_for: Duration::from_secs(3),
        })
        .await
        .expect("pressured worker should start");

    wait_until(
        || daemon.runtime().high_priority_shards.len() >= 2,
        "HighPriority policy scale-out with advisory disabled",
        Duration::from_secs(5),
    )
    .await;

    daemon.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn high_priority_policy_rollover_replaces_existing_generation_without_failure_semantics() {
    let _guard = TEST_LOCK.lock().await;
    reset_policy_test_state();
    let daemon = ServiceDaemon::builder()
        .with_registry(
            Registry::builder()
                .with_tag("__high_priority_runtime_policy__")
                .build(),
        )
        .build();

    daemon.run().await;
    wait_until(
        || HANDLE_READY.load(Ordering::SeqCst),
        "policy worker handle",
        Duration::from_secs(2),
    )
    .await;

    let instance = policy_worker_service_handle()
        .start(PolicyWorkerJob {
            block_for: Duration::from_secs(3),
        })
        .await
        .expect("pressured worker should start");

    wait_until(
        || {
            daemon
                .diagnostics_snapshot()
                .high_priority_placement_decisions
                .iter()
                .any(|decision| {
                    decision.kind == DiagnosticHighPriorityPlacementDecisionKind::Rollover
                })
        },
        "HighPriority policy rollover decision",
        Duration::from_secs(6),
    )
    .await;

    wait_until(
        || {
            instance
                .runtime()
                .is_some_and(|runtime| runtime.generation >= 2)
        },
        "HighPriority policy rollover generation replacement",
        Duration::from_secs(2),
    )
    .await;

    let runtime = instance
        .runtime()
        .expect("rolled-over instance runtime should be visible");
    assert!(
        runtime.generation >= 2,
        "policy rollover should advance the service generation"
    );
    let diagnostics = daemon.diagnostics_snapshot();
    let service = diagnostics
        .services
        .iter()
        .find(|service| service.service_instance_id == instance.instance_id())
        .expect("service diagnostics should be visible");
    assert!(
        service.aggregate.lifecycle.reload_requested > 0,
        "rollover should use the reload path"
    );
    assert_eq!(
        service.aggregate.lifecycle.recoverable_error, 0,
        "rollover should not be classified as a recoverable failure"
    );
    assert_eq!(
        service.aggregate.lifecycle.backoff_restart, 0,
        "rollover should not enter RestartPolicy backoff"
    );
    assert_eq!(
        service.aggregate.lifecycle.rate_limited_restart, 0,
        "rollover should not enter restart storm rate limiting"
    );

    daemon.shutdown();
}
