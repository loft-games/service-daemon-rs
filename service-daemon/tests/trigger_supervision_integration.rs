use service_daemon::{
    DiagnosticGenerationExitKind, DiagnosticLifecycleStats, ProviderError, Registry, RestartPolicy,
    ServiceDaemon, ServiceDiagnosticsSnapshot, TT::*, provider, trigger,
};
use std::sync::Arc;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;
use tokio::sync::Notify;
use tokio::time::timeout;

static RETRY_EXHAUSTION_ATTEMPTS: AtomicUsize = AtomicUsize::new(0);
static PANIC_DISPATCH_ATTEMPTS: AtomicUsize = AtomicUsize::new(0);
static TRIGGER_FATAL_PROVIDER_CALLED: AtomicBool = AtomicBool::new(false);
static SHUTDOWN_INFLIGHT_STARTED: LazyLock<Arc<Notify>> = LazyLock::new(|| Arc::new(Notify::new()));

#[provider(Notify)]
pub struct RetryExhaustionSignal;

#[trigger(
    Event(RetryExhaustionSignal),
    tags = ["__test_trigger_retry_exhaustion_supervision__"]
)]
async fn retry_exhaustion_trigger() -> anyhow::Result<()> {
    RETRY_EXHAUSTION_ATTEMPTS.fetch_add(1, Ordering::SeqCst);
    Err(anyhow::anyhow!("retry exhaustion integration failure"))
}

#[provider(Notify)]
pub struct PanicDispatchSignal;

#[trigger(
    Event(PanicDispatchSignal),
    tags = ["__test_trigger_panic_supervision__"]
)]
async fn panic_dispatch_trigger() -> anyhow::Result<()> {
    PANIC_DISPATCH_ATTEMPTS.fetch_add(1, Ordering::SeqCst);
    panic!("trigger dispatch panic integration failure");
}

#[derive(Clone, Default)]
pub struct TriggerFatalConfig;

#[provider]
async fn trigger_fatal_config() -> std::result::Result<TriggerFatalConfig, ProviderError> {
    TRIGGER_FATAL_PROVIDER_CALLED.store(true, Ordering::SeqCst);
    Err(ProviderError::Fatal(
        "trigger provider fatal integration failure".to_string(),
    ))
}

#[provider(Notify)]
pub struct ProviderFailureSignal;

#[trigger(
    Event(ProviderFailureSignal),
    tags = ["__test_trigger_provider_init_supervision__"]
)]
async fn provider_init_failure_trigger(_config: Arc<TriggerFatalConfig>) -> anyhow::Result<()> {
    Ok(())
}

#[provider(Notify)]
pub struct ShutdownInFlightSignal;

#[trigger(
    Event(ShutdownInFlightSignal),
    tags = ["__test_trigger_shutdown_inflight_supervision__"]
)]
async fn shutdown_inflight_trigger() -> anyhow::Result<()> {
    SHUTDOWN_INFLIGHT_STARTED.notify_one();
    futures::future::pending::<()>().await;
    Ok(())
}

fn phase9_policy() -> RestartPolicy {
    RestartPolicy::builder()
        .initial_delay(Duration::from_millis(10))
        .max_delay(Duration::from_millis(20))
        .jitter_factor(0.0)
        .trigger_max_retries(1)
        .wave_spawn_timeout(Duration::from_millis(200))
        .wave_stop_timeout(Duration::from_millis(300))
        .provider_init_timeout(Duration::from_secs(1))
        .build()
}

async fn wait_for_service_lifecycle<F>(
    daemon: &ServiceDaemon,
    service_name: &str,
    predicate: F,
) -> anyhow::Result<ServiceDiagnosticsSnapshot>
where
    F: Fn(&DiagnosticLifecycleStats) -> bool,
{
    timeout(Duration::from_secs(3), async {
        loop {
            if let Some(service) = daemon
                .diagnostics_snapshot()
                .services
                .into_iter()
                .find(|service| service.service_name == service_name)
                && predicate(&service.aggregate.lifecycle)
            {
                return service;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .map_err(Into::into)
}

#[tokio::test]
async fn test_trigger_dispatch_retry_exhaustion_restarts_and_records_diagnostics()
-> anyhow::Result<()> {
    RETRY_EXHAUSTION_ATTEMPTS.store(0, Ordering::SeqCst);

    let mut daemon = ServiceDaemon::builder()
        .with_registry(
            Registry::builder()
                .with_tag("__test_trigger_retry_exhaustion_supervision__")
                .build(),
        )
        .with_restart_policy(phase9_policy())
        .build();
    let cancel = daemon.cancel_token();
    daemon.run().await;
    tokio::time::sleep(Duration::from_millis(200)).await;

    RetryExhaustionSignal::resolve().await.notify();

    let service = wait_for_service_lifecycle(&daemon, "retry_exhaustion_trigger", |lifecycle| {
        lifecycle.recoverable_error >= 1 && lifecycle.backoff_restart >= 1
    })
    .await?;

    cancel.cancel();
    timeout(Duration::from_secs(5), daemon.wait()).await??;

    assert!(RETRY_EXHAUSTION_ATTEMPTS.load(Ordering::SeqCst) >= 1);
    assert_eq!(service.aggregate.lifecycle.recoverable_error, 1);
    assert_eq!(
        service.aggregate.lifecycle.last_exit_kind,
        Some(DiagnosticGenerationExitKind::RecoverableError)
    );

    Ok(())
}

#[tokio::test]
async fn test_trigger_dispatch_panic_restarts_and_records_panic_diagnostics() -> anyhow::Result<()>
{
    PANIC_DISPATCH_ATTEMPTS.store(0, Ordering::SeqCst);

    let mut daemon = ServiceDaemon::builder()
        .with_registry(
            Registry::builder()
                .with_tag("__test_trigger_panic_supervision__")
                .build(),
        )
        .with_restart_policy(phase9_policy())
        .build();
    let cancel = daemon.cancel_token();
    daemon.run().await;
    tokio::time::sleep(Duration::from_millis(200)).await;

    PanicDispatchSignal::resolve().await.notify();

    let service = wait_for_service_lifecycle(&daemon, "panic_dispatch_trigger", |lifecycle| {
        lifecycle.panic >= 1 && lifecycle.backoff_restart >= 1
    })
    .await?;

    cancel.cancel();
    timeout(Duration::from_secs(5), daemon.wait()).await??;

    assert!(PANIC_DISPATCH_ATTEMPTS.load(Ordering::SeqCst) >= 1);
    assert_eq!(service.aggregate.lifecycle.panic, 1);
    assert_eq!(
        service.aggregate.lifecycle.last_exit_kind,
        Some(DiagnosticGenerationExitKind::Panic)
    );

    Ok(())
}

#[tokio::test]
async fn test_trigger_provider_dependency_init_failure_shuts_down_daemon() -> anyhow::Result<()> {
    TRIGGER_FATAL_PROVIDER_CALLED.store(false, Ordering::SeqCst);

    let mut daemon = ServiceDaemon::builder()
        .with_registry(
            Registry::builder()
                .with_tag("__test_trigger_provider_init_supervision__")
                .build(),
        )
        .with_restart_policy(phase9_policy())
        .build();
    let shutdown = daemon.cancel_token();
    daemon.run().await;

    timeout(Duration::from_secs(5), shutdown.cancelled()).await?;
    timeout(Duration::from_secs(5), daemon.wait()).await??;

    assert!(TRIGGER_FATAL_PROVIDER_CALLED.load(Ordering::SeqCst));
    let service = daemon
        .diagnostics_snapshot()
        .services
        .into_iter()
        .find(|service| service.service_name == "provider_init_failure_trigger")
        .expect("trigger diagnostics should be recorded");
    assert_eq!(service.aggregate.lifecycle.provider_init_error, 1);
    assert_eq!(
        service.aggregate.lifecycle.last_exit_kind,
        Some(DiagnosticGenerationExitKind::ProviderInitError)
    );

    Ok(())
}

#[tokio::test]
async fn test_trigger_shutdown_with_in_flight_dispatch_does_not_record_recoverable_failure()
-> anyhow::Result<()> {
    let mut daemon = ServiceDaemon::builder()
        .with_registry(
            Registry::builder()
                .with_tag("__test_trigger_shutdown_inflight_supervision__")
                .build(),
        )
        .with_restart_policy(phase9_policy())
        .build();
    let cancel = daemon.cancel_token();
    daemon.run().await;
    tokio::time::sleep(Duration::from_millis(200)).await;

    ShutdownInFlightSignal::resolve().await.notify();
    timeout(Duration::from_secs(2), SHUTDOWN_INFLIGHT_STARTED.notified()).await?;

    cancel.cancel();
    timeout(Duration::from_secs(5), daemon.wait()).await??;

    let service = daemon
        .diagnostics_snapshot()
        .services
        .into_iter()
        .find(|service| service.service_name == "shutdown_inflight_trigger")
        .expect("trigger diagnostics should be recorded");
    assert_eq!(service.aggregate.lifecycle.recoverable_error, 0);
    assert_eq!(service.aggregate.lifecycle.panic, 0);
    assert_eq!(
        service.aggregate.lifecycle.last_exit_kind,
        Some(DiagnosticGenerationExitKind::Shutdown)
    );

    Ok(())
}
