use service_daemon::{ProviderError, ServiceDaemon, provider, service};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

static EAGER_INIT_CALLED: AtomicBool = AtomicBool::new(false);
static EAGER_FAILURE_INIT_CALLED: AtomicBool = AtomicBool::new(false);
static MISSING_ENV_SERVICE_ENTERED: AtomicBool = AtomicBool::new(false);
#[cfg(feature = "simulation")]
static SIMULATION_EAGER_INIT_CALLED: AtomicBool = AtomicBool::new(false);
#[cfg(feature = "simulation")]
static SIMULATION_EAGER_FAILURE_INIT_CALLED: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Default)]
pub struct EagerToken(pub String);

#[provider(eager = true)]
async fn eager_provider() -> EagerToken {
    EAGER_INIT_CALLED.store(true, Ordering::SeqCst);
    EagerToken("eager".to_string())
}

#[service(tags = ["stub_for_eager_test"])]
async fn stub_service(_token: Arc<EagerToken>) -> anyhow::Result<()> {
    service_daemon::done();

    while !service_daemon::is_shutdown() {
        service_daemon::sleep(Duration::from_millis(10)).await;
    }

    Ok(())
}

#[derive(Clone, Default)]
pub struct FailingEagerToken(pub String);

#[provider(eager = true)]
async fn failing_eager_provider() -> std::result::Result<FailingEagerToken, ProviderError> {
    EAGER_FAILURE_INIT_CALLED.store(true, Ordering::SeqCst);
    Err(ProviderError::Fatal(
        "intentional eager startup failure".to_string(),
    ))
}

#[service(tags = ["stub_for_eager_failure_test"])]
async fn failing_stub_service(_token: Arc<FailingEagerToken>) -> anyhow::Result<()> {
    Ok(())
}

#[derive(Clone)]
#[provider(
    env = "SERVICE_DAEMON_RS_TEST_REQUIRED_ENV_MISSING_5B9D1F6A",
    eager = true
)]
pub struct MissingEnvToken(pub String);

#[service(tags = ["stub_for_missing_env_failure_test"])]
async fn missing_env_stub_service(_token: Arc<MissingEnvToken>) -> anyhow::Result<()> {
    MISSING_ENV_SERVICE_ENTERED.store(true, Ordering::SeqCst);
    Ok(())
}

#[cfg(feature = "simulation")]
#[derive(Clone, Default)]
pub struct SimulationEagerToken(pub String);

#[cfg(feature = "simulation")]
#[provider(eager = true)]
async fn simulation_eager_provider() -> SimulationEagerToken {
    SIMULATION_EAGER_INIT_CALLED.store(true, Ordering::SeqCst);
    SimulationEagerToken("simulation eager".to_string())
}

#[cfg(feature = "simulation")]
#[service(tags = ["stub_for_simulation_eager_test"])]
async fn simulation_stub_service(_token: Arc<SimulationEagerToken>) -> anyhow::Result<()> {
    service_daemon::done();

    while !service_daemon::is_shutdown() {
        service_daemon::sleep(Duration::from_millis(10)).await;
    }

    Ok(())
}

#[cfg(feature = "simulation")]
#[derive(Clone, Default)]
pub struct SimulationFailingEagerToken(pub String);

#[cfg(feature = "simulation")]
#[provider(eager = true)]
async fn simulation_failing_eager_provider()
-> std::result::Result<SimulationFailingEagerToken, ProviderError> {
    SIMULATION_EAGER_FAILURE_INIT_CALLED.store(true, Ordering::SeqCst);
    Err(ProviderError::Fatal(
        "intentional simulation eager startup failure".to_string(),
    ))
}

#[cfg(feature = "simulation")]
#[service(tags = ["stub_for_simulation_eager_failure_test"])]
async fn simulation_failing_stub_service(
    _token: Arc<SimulationFailingEagerToken>,
) -> anyhow::Result<()> {
    Ok(())
}

#[tokio::test]
async fn test_async_fn_eager_init() {
    // Reset state for test
    EAGER_INIT_CALLED.store(false, Ordering::SeqCst);

    let mut daemon = ServiceDaemon::builder()
        .with_registry(
            service_daemon::models::Registry::builder()
                .with_tag("stub_for_eager_test")
                .build(),
        )
        .build();

    // Before run, eager provider should NOT be initialized
    assert!(!EAGER_INIT_CALLED.load(Ordering::SeqCst));

    // ServiceDaemon::run initializes eager providers before the event loop.
    let cancel = daemon.cancel_token();
    daemon.run().await;

    assert!(EAGER_INIT_CALLED.load(Ordering::SeqCst));

    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(5), daemon.wait())
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn test_async_fn_eager_init_failure_triggers_shutdown() {
    EAGER_FAILURE_INIT_CALLED.store(false, Ordering::SeqCst);

    let mut daemon = ServiceDaemon::builder()
        .with_registry(
            service_daemon::models::Registry::builder()
                .with_tag("stub_for_eager_failure_test")
                .build(),
        )
        .build();

    assert!(!EAGER_FAILURE_INIT_CALLED.load(Ordering::SeqCst));

    daemon.run().await;
    assert!(EAGER_FAILURE_INIT_CALLED.load(Ordering::SeqCst));
    assert!(daemon.cancel_token().is_cancelled());
}

#[tokio::test]
async fn test_missing_env_eager_provider_failure_triggers_shutdown() {
    assert!(std::env::var("SERVICE_DAEMON_RS_TEST_REQUIRED_ENV_MISSING_5B9D1F6A").is_err());
    MISSING_ENV_SERVICE_ENTERED.store(false, Ordering::SeqCst);

    let mut daemon = ServiceDaemon::builder()
        .with_registry(
            service_daemon::models::Registry::builder()
                .with_tag("stub_for_missing_env_failure_test")
                .build(),
        )
        .build();

    daemon.run().await;

    assert!(daemon.cancel_token().is_cancelled());
    assert!(!MISSING_ENV_SERVICE_ENTERED.load(Ordering::SeqCst));
}

#[cfg(feature = "simulation")]
#[tokio::test]
async fn test_run_for_duration_eager_init_matches_run_startup_boundary() {
    SIMULATION_EAGER_INIT_CALLED.store(false, Ordering::SeqCst);

    let daemon = ServiceDaemon::builder()
        .with_registry(
            service_daemon::models::Registry::builder()
                .with_tag("stub_for_simulation_eager_test")
                .build(),
        )
        .build();

    assert!(!SIMULATION_EAGER_INIT_CALLED.load(Ordering::SeqCst));

    daemon
        .run_for_duration(Duration::from_millis(100))
        .await
        .unwrap();

    assert!(SIMULATION_EAGER_INIT_CALLED.load(Ordering::SeqCst));
}

#[cfg(feature = "simulation")]
#[tokio::test]
async fn test_run_for_duration_eager_init_failure_returns_error() {
    SIMULATION_EAGER_FAILURE_INIT_CALLED.store(false, Ordering::SeqCst);

    let daemon = ServiceDaemon::builder()
        .with_registry(
            service_daemon::models::Registry::builder()
                .with_tag("stub_for_simulation_eager_failure_test")
                .build(),
        )
        .build();

    assert!(!SIMULATION_EAGER_FAILURE_INIT_CALLED.load(Ordering::SeqCst));

    let result = daemon.run_for_duration(Duration::from_millis(100)).await;

    assert!(SIMULATION_EAGER_FAILURE_INIT_CALLED.load(Ordering::SeqCst));
    assert!(result.is_err());
}
