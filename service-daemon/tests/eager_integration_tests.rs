use service_daemon::{ProviderError, ProviderInitError, ServiceDaemon, provider, service};
use std::ffi::OsString;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

const PARSE_ENV_NAME: &str = "SERVICE_DAEMON_RS_TEST_REQUIRED_ENV_PARSE_1E57D782";

static EAGER_INIT_CALLED: AtomicBool = AtomicBool::new(false);
static EAGER_FAILURE_INIT_CALLED: AtomicBool = AtomicBool::new(false);
static MISSING_ENV_SERVICE_ENTERED: AtomicBool = AtomicBool::new(false);
static PANICKING_EAGER_INIT_CALLED: AtomicBool = AtomicBool::new(false);
static PANICKING_EAGER_SERVICE_ENTERED: AtomicBool = AtomicBool::new(false);
static UNREACHABLE_EAGER_INIT_CALLED: AtomicBool = AtomicBool::new(false);
static TRANSITIVE_EAGER_DEP_INIT_CALLED: AtomicBool = AtomicBool::new(false);
static TRANSITIVE_PROVIDER_SAW_EAGER_DEP: AtomicBool = AtomicBool::new(false);
static TRANSITIVE_SERVICE_SAW_EAGER_DEP: AtomicBool = AtomicBool::new(false);
#[cfg(feature = "simulation")]
static SIMULATION_EAGER_INIT_CALLED: AtomicBool = AtomicBool::new(false);
#[cfg(feature = "simulation")]
static SIMULATION_EAGER_FAILURE_INIT_CALLED: AtomicBool = AtomicBool::new(false);
static ENV_VAR_LOCK: Mutex<()> = Mutex::new(());

struct EnvVarGuard {
    key: &'static str,
    previous: Option<OsString>,
    _lock: MutexGuard<'static, ()>,
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        unsafe {
            if let Some(previous) = &self.previous {
                std::env::set_var(self.key, previous);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }
}

fn set_test_env(key: &'static str, value: &'static str) -> EnvVarGuard {
    let lock = ENV_VAR_LOCK
        .lock()
        .unwrap_or_else(|err| panic!("env var test lock poisoned: {err}"));
    let previous = std::env::var_os(key);
    unsafe {
        std::env::set_var(key, value);
    }
    EnvVarGuard {
        key,
        previous,
        _lock: lock,
    }
}

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

#[derive(Clone, Debug)]
#[provider(
    env = "SERVICE_DAEMON_RS_TEST_REQUIRED_ENV_MISSING_5B9D1F6A",
    eager = true
)]
pub struct MissingEnvToken(pub String);

#[derive(Clone, Debug)]
#[provider(env = "SERVICE_DAEMON_RS_TEST_REQUIRED_ENV_PARSE_1E57D782")]
pub struct ParseEnvToken(pub u16);

#[service(tags = ["stub_for_missing_env_failure_test"])]
async fn missing_env_stub_service(_token: Arc<MissingEnvToken>) -> anyhow::Result<()> {
    MISSING_ENV_SERVICE_ENTERED.store(true, Ordering::SeqCst);
    Ok(())
}

#[derive(Clone, Default)]
pub struct PanickingEagerToken;

#[provider(eager = true)]
async fn panicking_eager_provider() -> PanickingEagerToken {
    PANICKING_EAGER_INIT_CALLED.store(true, Ordering::SeqCst);
    panic!("intentional eager provider panic")
}

#[service(tags = ["stub_for_panicking_eager_test"])]
async fn panicking_eager_stub_service(_token: Arc<PanickingEagerToken>) -> anyhow::Result<()> {
    PANICKING_EAGER_SERVICE_ENTERED.store(true, Ordering::SeqCst);
    Ok(())
}

#[derive(Clone, Default)]
pub struct UnreachableEagerToken;

#[provider(eager = true)]
async fn unreachable_eager_provider() -> UnreachableEagerToken {
    UNREACHABLE_EAGER_INIT_CALLED.store(true, Ordering::SeqCst);
    UnreachableEagerToken
}

#[service(tags = ["stub_without_eager_dependency"])]
async fn stub_without_eager_dependency() -> anyhow::Result<()> {
    service_daemon::done();

    while !service_daemon::is_shutdown() {
        service_daemon::sleep(Duration::from_millis(10)).await;
    }

    Ok(())
}

#[derive(Clone, Default)]
pub struct TransitiveEagerDependency;

#[provider(eager = true)]
async fn transitive_eager_dependency() -> TransitiveEagerDependency {
    TRANSITIVE_EAGER_DEP_INIT_CALLED.store(true, Ordering::SeqCst);
    TransitiveEagerDependency
}

#[derive(Clone, Default)]
pub struct TransitiveProvider {
    pub _dependency: Arc<TransitiveEagerDependency>,
}

#[provider]
async fn transitive_provider(dependency: Arc<TransitiveEagerDependency>) -> TransitiveProvider {
    TRANSITIVE_PROVIDER_SAW_EAGER_DEP.store(
        TRANSITIVE_EAGER_DEP_INIT_CALLED.load(Ordering::SeqCst),
        Ordering::SeqCst,
    );
    TransitiveProvider {
        _dependency: dependency,
    }
}

#[service(tags = ["stub_for_transitive_eager_dependency_test"])]
async fn transitive_eager_dependency_service(
    _provider: Arc<TransitiveProvider>,
) -> anyhow::Result<()> {
    TRANSITIVE_SERVICE_SAW_EAGER_DEP.store(
        TRANSITIVE_EAGER_DEP_INIT_CALLED.load(Ordering::SeqCst),
        Ordering::SeqCst,
    );
    service_daemon::done();

    while !service_daemon::is_shutdown() {
        service_daemon::sleep(Duration::from_millis(10)).await;
    }

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
            service_daemon::Registry::builder()
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
            service_daemon::Registry::builder()
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
async fn test_missing_env_public_helper_returns_fatal_error() {
    assert!(std::env::var("SERVICE_DAEMON_RS_TEST_REQUIRED_ENV_MISSING_5B9D1F6A").is_err());

    let result = MissingEnvToken::resolve().await;
    match result {
        Err(ProviderInitError::Fatal { provider, message }) => {
            assert_eq!(provider, "MissingEnvToken");
            assert!(
                message.contains("SERVICE_DAEMON_RS_TEST_REQUIRED_ENV_MISSING_5B9D1F6A"),
                "expected missing env name in Fatal message, got: {}",
                message
            );
        }
        other => panic!("Expected missing env Fatal, got {:?}", other),
    }
}

#[tokio::test]
async fn test_parse_env_public_helpers_preserve_error_boundary() {
    let _guard = set_test_env(PARSE_ENV_NAME, "not-a-u16");

    let snapshot_result = ParseEnvToken::resolve().await;
    match snapshot_result {
        Err(ProviderInitError::Fatal { provider, message }) => {
            assert_eq!(provider, "ParseEnvToken");
            assert!(
                message.contains(PARSE_ENV_NAME),
                "expected parse env name in Fatal message, got: {}",
                message
            );
            assert!(
                message.contains("cannot be parsed"),
                "expected parse failure in Fatal message, got: {}",
                message
            );
        }
        other => panic!("Expected parse env Fatal, got {:?}", other),
    }

    let rwlock_result = ParseEnvToken::resolve_rwlock().await;
    assert!(matches!(
        rwlock_result,
        Err(ProviderInitError::Fatal { provider, .. }) if provider == "ParseEnvToken"
    ));

    let mutex_result = ParseEnvToken::resolve_mutex().await;
    assert!(matches!(
        mutex_result,
        Err(ProviderInitError::Fatal { provider, .. }) if provider == "ParseEnvToken"
    ));

    let managed_result = ParseEnvToken::resolve_managed().await;
    match managed_result {
        Err(ProviderError::Fatal(message)) => {
            assert!(
                message.contains(PARSE_ENV_NAME),
                "expected parse env name in raw ProviderError, got: {}",
                message
            );
        }
        other => panic!("Expected raw managed ProviderError::Fatal, got {:?}", other),
    }
}

#[tokio::test]
async fn test_missing_env_eager_provider_failure_triggers_shutdown() {
    assert!(std::env::var("SERVICE_DAEMON_RS_TEST_REQUIRED_ENV_MISSING_5B9D1F6A").is_err());
    MISSING_ENV_SERVICE_ENTERED.store(false, Ordering::SeqCst);

    let mut daemon = ServiceDaemon::builder()
        .with_registry(
            service_daemon::Registry::builder()
                .with_tag("stub_for_missing_env_failure_test")
                .build(),
        )
        .build();

    daemon.run().await;

    assert!(daemon.cancel_token().is_cancelled());
    assert!(!MISSING_ENV_SERVICE_ENTERED.load(Ordering::SeqCst));
}

#[tokio::test]
async fn test_panicking_eager_provider_failure_triggers_shutdown() {
    PANICKING_EAGER_INIT_CALLED.store(false, Ordering::SeqCst);
    PANICKING_EAGER_SERVICE_ENTERED.store(false, Ordering::SeqCst);

    let mut daemon = ServiceDaemon::builder()
        .with_registry(
            service_daemon::Registry::builder()
                .with_tag("stub_for_panicking_eager_test")
                .build(),
        )
        .build();

    daemon.run().await;

    assert!(PANICKING_EAGER_INIT_CALLED.load(Ordering::SeqCst));
    assert!(daemon.cancel_token().is_cancelled());
    assert!(!PANICKING_EAGER_SERVICE_ENTERED.load(Ordering::SeqCst));
}

#[tokio::test]
async fn test_unreachable_eager_provider_is_not_initialized() {
    UNREACHABLE_EAGER_INIT_CALLED.store(false, Ordering::SeqCst);

    let mut daemon = ServiceDaemon::builder()
        .with_registry(
            service_daemon::Registry::builder()
                .with_tag("stub_without_eager_dependency")
                .build(),
        )
        .build();
    let cancel = daemon.cancel_token();

    daemon.run().await;

    assert!(!UNREACHABLE_EAGER_INIT_CALLED.load(Ordering::SeqCst));

    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(5), daemon.wait())
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn test_reachable_eager_provider_dependency_initializes_before_service() {
    TRANSITIVE_EAGER_DEP_INIT_CALLED.store(false, Ordering::SeqCst);
    TRANSITIVE_PROVIDER_SAW_EAGER_DEP.store(false, Ordering::SeqCst);
    TRANSITIVE_SERVICE_SAW_EAGER_DEP.store(false, Ordering::SeqCst);

    let mut daemon = ServiceDaemon::builder()
        .with_registry(
            service_daemon::Registry::builder()
                .with_tag("stub_for_transitive_eager_dependency_test")
                .build(),
        )
        .build();
    let cancel = daemon.cancel_token();

    daemon.run().await;

    assert!(TRANSITIVE_EAGER_DEP_INIT_CALLED.load(Ordering::SeqCst));
    assert!(TRANSITIVE_PROVIDER_SAW_EAGER_DEP.load(Ordering::SeqCst));
    assert!(TRANSITIVE_SERVICE_SAW_EAGER_DEP.load(Ordering::SeqCst));

    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(5), daemon.wait())
        .await
        .unwrap()
        .unwrap();
}

#[cfg(feature = "simulation")]
#[tokio::test]
async fn test_run_for_duration_eager_init_matches_run_startup_boundary() {
    SIMULATION_EAGER_INIT_CALLED.store(false, Ordering::SeqCst);

    let daemon = ServiceDaemon::builder()
        .with_registry(
            service_daemon::Registry::builder()
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
            service_daemon::Registry::builder()
                .with_tag("stub_for_simulation_eager_failure_test")
                .build(),
        )
        .build();

    assert!(!SIMULATION_EAGER_FAILURE_INIT_CALLED.load(Ordering::SeqCst));

    let result = daemon.run_for_duration(Duration::from_millis(100)).await;

    assert!(SIMULATION_EAGER_FAILURE_INIT_CALLED.load(Ordering::SeqCst));
    assert!(result.is_err());
}
