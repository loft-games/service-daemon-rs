use futures::FutureExt;
use service_daemon::{
    DiagnosticGenerationExitKind, DiagnosticProviderFailureBoundaryKind,
    DiagnosticProviderFailureKind, DiagnosticProviderFailureRuntimePhase,
    DiagnosticProviderFailureSourceKind, ProviderError, ProviderInitError, Registry, RestartPolicy,
    ServiceDaemon, TT::*, provider, service, trigger,
};
use std::any::Any;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, LazyLock, Mutex as StdMutex, MutexGuard, Once};
use std::time::Duration;
use tokio::time::timeout;
use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;
use tracing_subscriber::prelude::*;

static DEPENDENCY_FATAL_CALLED: AtomicBool = AtomicBool::new(false);
static DEPENDENCY_FATAL_PARENT_ENTERED: AtomicBool = AtomicBool::new(false);
static DEPENDENCY_FATAL_SERVICE_ENTERED: AtomicBool = AtomicBool::new(false);
static DEPENDENCY_TIMEOUT_ATTEMPTS: AtomicU32 = AtomicU32::new(0);
static DEPENDENCY_TIMEOUT_PARENT_ENTERED: AtomicBool = AtomicBool::new(false);
static DEPENDENCY_TIMEOUT_SERVICE_ENTERED: AtomicBool = AtomicBool::new(false);
static WATCH_TARGET_PROVIDER_CALLED: AtomicBool = AtomicBool::new(false);
static WATCH_TARGET_TRIGGER_ENTERED: AtomicBool = AtomicBool::new(false);

const TRACE_PARSE_ENV_NAME: &str = "SERVICE_DAEMON_RS_TEST_PHASE16_SOURCE_PARSE_3A889E90";

static TRACE_INIT: Once = Once::new();
static TRACE_CAPTURE: LazyLock<CapturedTraceFields> = LazyLock::new(CapturedTraceFields::default);
static TRACE_LOCK: LazyLock<tokio::sync::Mutex<()>> = LazyLock::new(|| tokio::sync::Mutex::new(()));
static ENV_VAR_LOCK: LazyLock<StdMutex<()>> = LazyLock::new(|| StdMutex::new(()));

#[derive(Clone, Default)]
struct CapturedTraceFields {
    events: Arc<StdMutex<Vec<BTreeMap<String, String>>>>,
}

#[derive(Default)]
struct TraceFieldVisitor {
    fields: BTreeMap<String, String>,
}

impl Visit for TraceFieldVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.fields
            .insert(field.name().to_string(), format!("{value:?}"));
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        self.fields
            .insert(field.name().to_string(), value.to_string());
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.fields
            .insert(field.name().to_string(), value.to_string());
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.fields
            .insert(field.name().to_string(), value.to_string());
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.fields
            .insert(field.name().to_string(), value.to_string());
    }
}

impl<S> Layer<S> for CapturedTraceFields
where
    S: Subscriber,
{
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let mut visitor = TraceFieldVisitor::default();
        event.record(&mut visitor);
        self.events
            .lock()
            .unwrap_or_else(|err| panic!("trace capture lock poisoned: {err}"))
            .push(visitor.fields);
    }
}

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

fn install_trace_capture() {
    TRACE_INIT.call_once(|| {
        let subscriber = tracing_subscriber::registry().with(TRACE_CAPTURE.clone());
        tracing::subscriber::set_global_default(subscriber)
            .unwrap_or_else(|_| panic!("trace capture subscriber should install once"));
        tracing::callsite::rebuild_interest_cache();
    });
}

fn clear_trace_events() {
    TRACE_CAPTURE
        .events
        .lock()
        .unwrap_or_else(|err| panic!("trace capture lock poisoned: {err}"))
        .clear();
}

fn trace_events() -> Vec<BTreeMap<String, String>> {
    TRACE_CAPTURE
        .events
        .lock()
        .unwrap_or_else(|err| panic!("trace capture lock poisoned: {err}"))
        .clone()
}

fn assert_trace_source(kind: &str) {
    let events = trace_events();
    assert!(
        events
            .iter()
            .any(|event| { event.get("provider_init_source_kind") == Some(&kind.to_owned()) }),
        "expected provider_init_source_kind={kind}, got events: {events:?}"
    );
}

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

#[derive(Clone, Debug)]
#[provider(env = "SERVICE_DAEMON_RS_TEST_PHASE16_SOURCE_PARSE_3A889E90")]
pub struct TraceParseEnvToken(pub u16);

#[derive(Clone, Default)]
pub struct PanicSourceProvider;

#[provider]
async fn panic_source_provider() -> PanicSourceProvider {
    panic!("phase16 typed source panic")
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

fn panic_payload_message(payload: Box<dyn Any + Send>) -> String {
    match payload.downcast::<String>() {
        Ok(message) => *message,
        Err(payload) => match payload.downcast::<&'static str>() {
            Ok(message) => (*message).to_owned(),
            Err(_) => "non-string panic payload".to_owned(),
        },
    }
}

#[tokio::test(flavor = "current_thread")]
async fn test_required_env_parse_emits_environment_parse_source() {
    let _trace_guard = TRACE_LOCK.lock().await;
    install_trace_capture();
    clear_trace_events();
    let _env_guard = set_test_env(TRACE_PARSE_ENV_NAME, "not-a-u16");

    let result = TraceParseEnvToken::resolve().await;

    assert!(matches!(
        result,
        Err(ProviderInitError::Fatal { provider, .. }) if provider == "TraceParseEnvToken"
    ));
    assert_trace_source("environment_parse");
}

#[tokio::test(flavor = "current_thread")]
async fn test_dependency_provider_failure_emits_dependency_source() {
    let _trace_guard = TRACE_LOCK.lock().await;
    install_trace_capture();
    clear_trace_events();
    DEPENDENCY_FATAL_CALLED.store(false, Ordering::SeqCst);
    DEPENDENCY_FATAL_PARENT_ENTERED.store(false, Ordering::SeqCst);

    let result = FatalParentProvider::resolve().await;

    assert!(DEPENDENCY_FATAL_CALLED.load(Ordering::SeqCst));
    assert!(!DEPENDENCY_FATAL_PARENT_ENTERED.load(Ordering::SeqCst));
    assert!(matches!(result, Err(ProviderInitError::Fatal { .. })));
    assert_trace_source("user_provider_fatal");
    assert_trace_source("dependency_provider");
}

#[tokio::test(flavor = "current_thread")]
async fn test_provider_panic_emits_panic_source() {
    let _trace_guard = TRACE_LOCK.lock().await;
    install_trace_capture();
    clear_trace_events();

    let result = <PanicSourceProvider as service_daemon::Provided>::resolve().await;

    assert!(matches!(
        result,
        Err(ProviderInitError::Fatal { provider, .. }) if provider == "PanicSourceProvider"
    ));
    assert_trace_source("panic");
}

#[tokio::test(flavor = "current_thread")]
async fn test_infallible_helper_panic_message_points_to_provider_definition() {
    let payload = match AssertUnwindSafe(PanicSourceProvider::resolve())
        .catch_unwind()
        .await
    {
        Ok(_) => panic!("direct helper should panic after provider init panic"),
        Err(payload) => payload,
    };
    let message = panic_payload_message(payload);

    assert!(
        message.contains("provider `PanicSourceProvider` failed in direct helper `resolve`"),
        "panic message should name the provider type and generated helper: {message}"
    );
    assert!(
        message.contains("provider_origin=#[provider] function panic_source_provider"),
        "panic message should point to the provider function: {message}"
    );
    assert!(
        message.contains(
            "provider_defined_at=service-daemon/tests/provider_init_boundary_integration_tests.rs:"
        ),
        "panic message should include the provider definition file: {message}"
    );
    assert!(
        message.contains(
            "helper_called_at=service-daemon/tests/provider_init_boundary_integration_tests.rs:"
        ),
        "panic message should include the direct helper callsite: {message}"
    );
    assert!(
        message.contains("phase16 typed source panic"),
        "panic message should preserve the original user panic text: {message}"
    );
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
    let diagnostics = daemon.diagnostics_snapshot();
    let failure = diagnostics
        .provider_failures
        .iter()
        .find(|failure| {
            failure.provider == "TimeoutLeafProvider"
                && failure.source
                    == DiagnosticProviderFailureSourceKind::UserProviderRetryableTimeout
        })
        .expect("runtime retryable timeout should be projected");
    assert_eq!(
        failure.phase,
        DiagnosticProviderFailureRuntimePhase::ServiceGenerationResolve
    );
    assert_eq!(
        failure.boundary,
        DiagnosticProviderFailureBoundaryKind::SnapshotResolve
    );
    assert_eq!(failure.failure_kind, DiagnosticProviderFailureKind::Timeout);
    let retry = failure
        .retry
        .as_ref()
        .expect("retry timeout should project retry diagnostics");
    assert!(retry.attempts > 0);
    assert!(!retry.recent_errors.is_empty());

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
