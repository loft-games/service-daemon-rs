#![cfg(feature = "high-priority")]

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
    observe_sleep: bool,
}

#[derive(Clone)]
struct PolicyWorkerHandle(ServiceHandle);

#[service(
    tags = ["__high_priority_runtime_policy__"],
    scheduling = HighPriority
)]
async fn policy_worker(#[input] job: &PolicyWorkerJob) -> anyhow::Result<()> {
    WORKER_STARTS.fetch_add(1, Ordering::SeqCst);
    if job.observe_sleep {
        done();
        loop {
            let completed = tokio::select! {
                biased;
                completed = service_daemon::sleep(Duration::from_millis(5)) => completed,
                _ = async {
                    std::thread::sleep(job.block_for);
                    std::future::pending::<()>().await;
                } => false,
            };
            if !completed {
                break;
            }
        }
    } else {
        if !job.block_for.is_zero() {
            std::thread::sleep(job.block_for);
        }
        done();
        wait_shutdown().await;
    }
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
    let original_shard_id = daemon.runtime().high_priority_shards[0].shard_id;

    let handle = policy_worker_service_handle();
    let pressured = handle
        .start(PolicyWorkerJob {
            block_for: Duration::from_millis(400),
            observe_sleep: true,
        })
        .await
        .expect("pressured worker should start");

    wait_until(
        || daemon.runtime().high_priority_shards.len() >= 2,
        "HighPriority policy scale-out",
        Duration::from_secs(10),
    )
    .await;

    let placed_after_scale_out = handle
        .start(PolicyWorkerJob {
            block_for: Duration::ZERO,
            observe_sleep: false,
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

    let placed_runtime = placed_after_scale_out
        .runtime()
        .expect("post-scale worker runtime should be visible");
    assert_ne!(
        Some(original_shard_id),
        placed_runtime.high_priority_shard_id,
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
    assert!(
        diagnostics.services.iter().any(|service| {
            service.service_instance_id == pressured.instance_id()
                && service.aggregate.service_sleep.completed >= 3
        }),
        "scale-out requires completed ServiceSleep evidence for the pressured instance"
    );

    daemon.shutdown();
    daemon
        .wait()
        .await
        .expect("daemon should shut down cleanly");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn high_priority_policy_does_not_scale_blocked_shard_without_service_sleep_evidence() {
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
            observe_sleep: false,
        })
        .await
        .expect("pressured worker should start");

    wait_until(
        || {
            instance
                .runtime()
                .is_some_and(|runtime| runtime.status == ServiceStatus::Healthy)
                && daemon
                    .diagnostics_snapshot()
                    .high_priority_shards
                    .iter()
                    .any(|shard| {
                        shard.aggregate.runtime_probe.completed >= 3
                            && shard.aggregate.runtime_probe.max_drift_ms >= 100
                    })
        },
        "blocked worker completion and shard probe pressure evidence",
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(daemon.runtime().high_priority_shards.len(), 1);
    let diagnostics = daemon.diagnostics_snapshot();
    let service = diagnostics
        .services
        .iter()
        .find(|service| service.service_instance_id == instance.instance_id())
        .expect("blocked service should have diagnostics");
    assert_eq!(service.aggregate.service_sleep.completed, 0);
    assert!(
        diagnostics
            .high_priority_placement_decisions
            .iter()
            .all(|decision| {
                decision.kind != DiagnosticHighPriorityPlacementDecisionKind::ScaleOut
                    && decision.kind != DiagnosticHighPriorityPlacementDecisionKind::Rollover
            })
    );

    daemon.shutdown();
    daemon
        .wait()
        .await
        .expect("daemon should shut down cleanly");
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
            block_for: Duration::from_millis(400),
            observe_sleep: true,
        })
        .await
        .expect("pressured worker should start");

    wait_until(
        || daemon.runtime().high_priority_shards.len() >= 2,
        "HighPriority policy scale-out with advisory disabled",
        Duration::from_secs(10),
    )
    .await;

    daemon.shutdown();
    daemon
        .wait()
        .await
        .expect("daemon should shut down cleanly");
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
            block_for: Duration::from_millis(400),
            observe_sleep: true,
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
        Duration::from_secs(10),
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
    daemon
        .wait()
        .await
        .expect("daemon should shut down cleanly");
}

mod timeout_reload {
    use super::*;
    use futures::FutureExt;
    use service_daemon::{DaemonInstanceHandle, ManagedProvided, ServiceInstanceId};
    use std::collections::BTreeMap;
    use std::panic::AssertUnwindSafe;
    use std::sync::{Arc, Once};
    use tracing::field::{Field, Visit};
    use tracing::{Event, Subscriber};
    use tracing_subscriber::Layer;
    use tracing_subscriber::layer::Context;
    use tracing_subscriber::prelude::*;

    static STARTS: AtomicUsize = AtomicUsize::new(0);
    static OLD_GENERATION_WAITING: AtomicBool = AtomicBool::new(false);
    static RELEASE_OLD_GENERATION: LazyLock<tokio::sync::Notify> =
        LazyLock::new(tokio::sync::Notify::new);
    static TRACE_INIT: Once = Once::new();
    static TRACE: LazyLock<RuntimeTrace> = LazyLock::new(RuntimeTrace::default);

    #[derive(Clone, Default)]
    #[provider]
    struct TimeoutReloadConfig {
        revision: u64,
    }

    #[service(tags = ["__high_priority_timeout_reload__"], scheduling = HighPriority)]
    async fn timeout_reload_worker(_config: Arc<TimeoutReloadConfig>) -> anyhow::Result<()> {
        let generation = STARTS.fetch_add(1, Ordering::SeqCst) + 1;
        done();
        loop {
            let completed = tokio::select! {
                biased;
                completed = service_daemon::sleep(Duration::from_millis(5)) => completed,
                _ = async {
                    std::thread::sleep(Duration::from_millis(400));
                    std::future::pending::<()>().await;
                } => false,
            };
            if !completed {
                break;
            }
        }
        if generation == 1 {
            OLD_GENERATION_WAITING.store(true, Ordering::SeqCst);
            RELEASE_OLD_GENERATION.notified().await;
        }
        Ok(())
    }

    #[derive(Clone, Default)]
    struct RuntimeTrace(Arc<Mutex<Vec<BTreeMap<String, String>>>>);

    #[derive(Default)]
    struct Fields(BTreeMap<String, String>);

    impl Visit for Fields {
        fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
            self.0.insert(field.name().into(), format!("{value:?}"));
        }

        fn record_str(&mut self, field: &Field, value: &str) {
            self.0.insert(field.name().into(), value.into());
        }

        fn record_u64(&mut self, field: &Field, value: u64) {
            self.0.insert(field.name().into(), value.to_string());
        }
    }

    impl<S: Subscriber> Layer<S> for RuntimeTrace {
        fn on_event(&self, event: &Event<'_>, _context: Context<'_, S>) {
            if event.metadata().target()
                != "service_daemon::core::service_daemon::high_priority::runtime"
                || *event.metadata().level() == tracing::Level::DEBUG
            {
                return;
            }
            let mut fields = Fields::default();
            event.record(&mut fields);
            self.0.lock().expect("runtime trace lock").push(fields.0);
        }
    }

    fn events(instance: ServiceInstanceId, message: &str) -> Vec<BTreeMap<String, String>> {
        TRACE
            .0
            .lock()
            .expect("runtime trace lock")
            .iter()
            .filter(|fields| {
                fields.get("service_instance_id") == Some(&instance.to_string())
                    && fields.get("message").is_some_and(|value| value == message)
            })
            .cloned()
            .collect()
    }

    fn requests(instance: ServiceInstanceId) -> Vec<BTreeMap<String, String>> {
        events(instance, "HighPriority resource intervention requested")
    }

    fn pauses(instance: ServiceInstanceId) -> Vec<BTreeMap<String, String>> {
        events(
            instance,
            "HighPriority expansion paused; new comparable evidence is required",
        )
    }

    fn worker(daemon: &DaemonInstanceHandle) -> service_daemon::ServiceDiagnosticsSnapshot {
        daemon
            .diagnostics_snapshot()
            .services
            .into_iter()
            .find(|service| service.service_name == "timeout_reload_worker")
            .expect("timeout worker diagnostics")
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn late_policy_generation_stays_paused_until_external_provider_reload() {
        let _guard = TEST_LOCK.lock().await;
        TRACE_INIT.call_once(|| {
            tracing::subscriber::set_global_default(
                tracing_subscriber::registry().with(TRACE.clone()),
            )
            .expect("install integration trace capture");
        });
        TRACE.0.lock().expect("runtime trace lock").clear();
        STARTS.store(0, Ordering::SeqCst);
        OLD_GENERATION_WAITING.store(false, Ordering::SeqCst);
        let daemon = ServiceDaemon::builder()
            .with_registry(
                Registry::builder()
                    .with_tag("__high_priority_timeout_reload__")
                    .build(),
            )
            .build();
        daemon.run().await;

        let result = AssertUnwindSafe(async {
            wait_until(|| OLD_GENERATION_WAITING.load(Ordering::SeqCst),
                "generation 1 receiving policy reload", Duration::from_secs(15)).await;
            let instance = worker(&daemon).service_instance_id;
            assert_eq!(STARTS.load(Ordering::SeqCst), 1);
            assert_eq!(requests(instance).len(), 1);
            let shards_after_intervention = daemon.runtime().high_priority_shards.len();
            assert_eq!(shards_after_intervention, 2);

            wait_until(|| !pauses(instance).is_empty(),
                "real 120-second intervention timeout", Duration::from_secs(135)).await;
            let pause = pauses(instance);
            assert_eq!(pause.len(), 1);
            assert_eq!(pause[0].get("reason").map(String::as_str), Some("TimedOut"));
            assert_eq!(pause[0].get("before_generation").map(String::as_str), Some("1"));
            assert_eq!(worker(&daemon).current_generation, 1);
            assert_eq!(STARTS.load(Ordering::SeqCst), 1);

            RELEASE_OLD_GENERATION.notify_one();
            wait_until(|| STARTS.load(Ordering::SeqCst) >= 2,
                "late generation 2 body", Duration::from_secs(5)).await;
            let observation_deadline = tokio::time::Instant::now() + Duration::from_secs(10);
            while tokio::time::Instant::now() < observation_deadline {
                assert_eq!(worker(&daemon).current_generation, 2, "late policy generation must stay paused");
                assert_eq!(requests(instance).len(), 1, "no repeated resource intervention while paused");
                assert_eq!(daemon.runtime().high_priority_shards.len(), shards_after_intervention);
                assert_eq!(pauses(instance).len(), 1, "polling must not repeat the pause warning");
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            let diagnostics = daemon.diagnostics_snapshot();
            let late_generation = diagnostics.generations.iter()
                .find(|generation| generation.service_instance_id == instance && generation.generation == 2)
                .expect("late generation diagnostics");
            assert!(late_generation.aggregate.service_sleep.completed >= 12, "late generation must supply fresh pressure samples");
            assert!(late_generation.aggregate.service_sleep.avg_drift_ms >= 100);
            assert!(diagnostics.high_priority_shards.iter().any(|shard|
                Some(shard.shard_id) != late_generation.high_priority_shard_id
                && shard.pressure_state == service_daemon::HighPriorityShardPressureState::Nominal),
                "a nominal alternative shard must exist so capacity cannot mask an unintended rollover");

            {
                let config = <TimeoutReloadConfig as ManagedProvided>::resolve_rwlock().await
                    .expect("resolve external reload config");
                config.write().await.revision += 1;
            }
            wait_until(|| STARTS.load(Ordering::SeqCst) >= 3,
                "external provider reload generation", Duration::from_secs(5)).await;
            wait_until(|| requests(instance).iter().any(|request|
                request.get("generation").map(String::as_str) == Some("3")),
                "policy evaluation resumed for external generation 3", Duration::from_secs(20)).await;
            wait_until(|| STARTS.load(Ordering::SeqCst) >= 4,
                "resumed policy reload generation", Duration::from_secs(5)).await;
            assert_eq!(pauses(instance).len(), 1);
            let lifecycle = worker(&daemon).aggregate.lifecycle;
            assert_eq!(lifecycle.recoverable_error, 0);
            assert_eq!(lifecycle.backoff_restart, 0);
            assert_eq!(lifecycle.rate_limited_restart, 0);
            assert!(lifecycle.reload_requested >= 3);
        }).catch_unwind().await;

        RELEASE_OLD_GENERATION.notify_one();
        daemon.shutdown();
        let shutdown = tokio::time::timeout(Duration::from_secs(10), daemon.wait()).await;
        if let Err(payload) = result {
            std::panic::resume_unwind(payload);
        }
        shutdown
            .expect("daemon shutdown deadline")
            .expect("clean daemon shutdown");
    }
}
