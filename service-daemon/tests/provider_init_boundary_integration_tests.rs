use service_daemon::{
    DiagnosticGenerationExitKind, ProviderError, Registry, RestartPolicy, ServiceDaemon, TT::*,
    provider, service, trigger,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;
use tokio::time::timeout;

static DEPENDENCY_FATAL_CALLED: AtomicBool = AtomicBool::new(false);
static DEPENDENCY_FATAL_PARENT_ENTERED: AtomicBool = AtomicBool::new(false);
static DEPENDENCY_FATAL_SERVICE_ENTERED: AtomicBool = AtomicBool::new(false);
static DEPENDENCY_TIMEOUT_ATTEMPTS: AtomicU32 = AtomicU32::new(0);
static DEPENDENCY_TIMEOUT_PARENT_ENTERED: AtomicBool = AtomicBool::new(false);
static DEPENDENCY_TIMEOUT_SERVICE_ENTERED: AtomicBool = AtomicBool::new(false);
static WATCH_TARGET_PROVIDER_CALLED: AtomicBool = AtomicBool::new(false);
static WATCH_TARGET_TRIGGER_ENTERED: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Default)]
pub struct FatalLeafProvider;

#[provider]
async fn fatal_leaf_provider() -> std::result::Result<FatalLeafProvider, ProviderError> {
    DEPENDENCY_FATAL_CALLED.store(true, Ordering::SeqCst);
    Err(ProviderError::Fatal(
        "dependency provider fatal boundary".to_owned(),
    ))
}

#[derive(Clone, Default)]
pub struct FatalParentProvider;

#[provider]
async fn fatal_parent_provider(_leaf: Arc<FatalLeafProvider>) -> FatalParentProvider {
    DEPENDENCY_FATAL_PARENT_ENTERED.store(true, Ordering::SeqCst);
    FatalParentProvider
}

#[service(tags = ["__phase16_dependency_provider_fatal__"])]
async fn dependency_provider_fatal_service(
    _provider: Arc<FatalParentProvider>,
) -> anyhow::Result<()> {
    DEPENDENCY_FATAL_SERVICE_ENTERED.store(true, Ordering::SeqCst);
    Ok(())
}

#[derive(Clone, Default)]
pub struct TimeoutLeafProvider;

#[provider]
async fn timeout_leaf_provider() -> std::result::Result<TimeoutLeafProvider, ProviderError> {
    DEPENDENCY_TIMEOUT_ATTEMPTS.fetch_add(1, Ordering::SeqCst);
    Err(ProviderError::Retryable(
        "dependency provider retry boundary".to_owned(),
    ))
}

#[derive(Clone, Default)]
pub struct TimeoutParentProvider;

#[provider]
async fn timeout_parent_provider(_leaf: Arc<TimeoutLeafProvider>) -> TimeoutParentProvider {
    DEPENDENCY_TIMEOUT_PARENT_ENTERED.store(true, Ordering::SeqCst);
    TimeoutParentProvider
}

#[service(tags = ["__phase16_dependency_provider_timeout__"])]
async fn dependency_provider_timeout_service(
    _provider: Arc<TimeoutParentProvider>,
) -> anyhow::Result<()> {
    DEPENDENCY_TIMEOUT_SERVICE_ENTERED.store(true, Ordering::SeqCst);
    Ok(())
}

#[derive(Clone, Default)]
pub struct FailingWatchTarget;

#[provider]
async fn failing_watch_target() -> std::result::Result<FailingWatchTarget, ProviderError> {
    WATCH_TARGET_PROVIDER_CALLED.store(true, Ordering::SeqCst);
    Err(ProviderError::Fatal(
        "watch target provider fatal boundary".to_owned(),
    ))
}

#[trigger(Watch(FailingWatchTarget), tags = ["__phase16_watch_target_failure__"])]
async fn watch_target_failure_trigger(_target: Arc<FailingWatchTarget>) -> anyhow::Result<()> {
    WATCH_TARGET_TRIGGER_ENTERED.store(true, Ordering::SeqCst);
    Ok(())
}

fn phase16_policy() -> RestartPolicy {
    RestartPolicy::builder()
        .initial_delay(Duration::from_millis(5))
        .max_delay(Duration::from_millis(10))
        .jitter_factor(0.0)
        .wave_spawn_timeout(Duration::from_millis(100))
        .provider_init_timeout(Duration::from_millis(60))
        .wave_stop_timeout(Duration::from_millis(200))
        .build()
}

async fn run_until_provider_init_shutdown(tag: &'static str) -> anyhow::Result<ServiceDaemon> {
    let mut daemon = ServiceDaemon::builder()
        .with_registry(Registry::builder().with_tag(tag).build())
        .with_restart_policy(phase16_policy())
        .build();
    let shutdown = daemon.cancel_token();

    daemon.run().await;
    timeout(Duration::from_secs(5), shutdown.cancelled()).await?;
    timeout(Duration::from_secs(5), daemon.wait()).await??;

    Ok(daemon)
}

fn assert_provider_init_exit(daemon: &ServiceDaemon, service_name: &str) {
    let diagnostics = daemon.diagnostics_snapshot();
    let service = diagnostics
        .services
        .iter()
        .find(|service| service.service_name == service_name)
        .unwrap_or_else(|| panic!("diagnostics for {service_name} should be recorded"));

    assert_eq!(service.aggregate.lifecycle.provider_init_error, 1);
    assert_eq!(
        service.aggregate.lifecycle.last_exit_kind,
        Some(DiagnosticGenerationExitKind::ProviderInitError)
    );
    assert_eq!(service.aggregate.lifecycle.last_restart_decision, None);
}

#[tokio::test]
async fn test_dependency_provider_fatal_shuts_down_daemon() -> anyhow::Result<()> {
    DEPENDENCY_FATAL_CALLED.store(false, Ordering::SeqCst);
    DEPENDENCY_FATAL_PARENT_ENTERED.store(false, Ordering::SeqCst);
    DEPENDENCY_FATAL_SERVICE_ENTERED.store(false, Ordering::SeqCst);

    let daemon = run_until_provider_init_shutdown("__phase16_dependency_provider_fatal__").await?;

    assert!(DEPENDENCY_FATAL_CALLED.load(Ordering::SeqCst));
    assert!(!DEPENDENCY_FATAL_PARENT_ENTERED.load(Ordering::SeqCst));
    assert!(!DEPENDENCY_FATAL_SERVICE_ENTERED.load(Ordering::SeqCst));
    assert_provider_init_exit(&daemon, "dependency_provider_fatal_service");

    Ok(())
}

#[tokio::test]
async fn test_dependency_provider_retry_timeout_shuts_down_daemon() -> anyhow::Result<()> {
    DEPENDENCY_TIMEOUT_ATTEMPTS.store(0, Ordering::SeqCst);
    DEPENDENCY_TIMEOUT_PARENT_ENTERED.store(false, Ordering::SeqCst);
    DEPENDENCY_TIMEOUT_SERVICE_ENTERED.store(false, Ordering::SeqCst);

    let daemon =
        run_until_provider_init_shutdown("__phase16_dependency_provider_timeout__").await?;

    assert!(DEPENDENCY_TIMEOUT_ATTEMPTS.load(Ordering::SeqCst) > 0);
    assert!(!DEPENDENCY_TIMEOUT_PARENT_ENTERED.load(Ordering::SeqCst));
    assert!(!DEPENDENCY_TIMEOUT_SERVICE_ENTERED.load(Ordering::SeqCst));
    assert_provider_init_exit(&daemon, "dependency_provider_timeout_service");

    Ok(())
}

#[tokio::test]
async fn test_watch_trigger_target_provider_failure_shuts_down_daemon() -> anyhow::Result<()> {
    WATCH_TARGET_PROVIDER_CALLED.store(false, Ordering::SeqCst);
    WATCH_TARGET_TRIGGER_ENTERED.store(false, Ordering::SeqCst);

    let daemon = run_until_provider_init_shutdown("__phase16_watch_target_failure__").await?;

    assert!(WATCH_TARGET_PROVIDER_CALLED.load(Ordering::SeqCst));
    assert!(!WATCH_TARGET_TRIGGER_ENTERED.load(Ordering::SeqCst));
    assert_provider_init_exit(&daemon, "watch_target_failure_trigger");

    Ok(())
}
