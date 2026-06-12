use crate::core::context;
use crate::core::diagnostics::{
    ProviderFailureBoundaryKind, ProviderFailureKind, ProviderFailureRetryDiagnosticsSnapshot,
    ProviderFailureRuntimePhase, ProviderFailureSnapshot, ProviderFailureSourceKind,
};
use crate::models::{
    BackoffController, ProviderError, ProviderInitError, RestartPolicy, ServiceStatus,
};
use futures::FutureExt;
use std::any::Any;
use std::collections::VecDeque;
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::task_local;
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

const PROVIDER_INIT_RECENT_ERROR_LIMIT: usize = 4;

#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderInitBoundaryKind {
    SnapshotResolve,
    RwLockResolve,
    MutexResolve,
    EagerInit,
    FrameworkValidation,
}

impl ProviderInitBoundaryKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::SnapshotResolve => "snapshot_resolve",
            Self::RwLockResolve => "rwlock_resolve",
            Self::MutexResolve => "mutex_resolve",
            Self::EagerInit => "eager_init",
            Self::FrameworkValidation => "framework_validation",
        }
    }
}

impl From<ProviderInitBoundaryKind> for ProviderFailureBoundaryKind {
    fn from(value: ProviderInitBoundaryKind) -> Self {
        match value {
            ProviderInitBoundaryKind::SnapshotResolve => Self::SnapshotResolve,
            ProviderInitBoundaryKind::RwLockResolve => Self::RwLockResolve,
            ProviderInitBoundaryKind::MutexResolve => Self::MutexResolve,
            ProviderInitBoundaryKind::EagerInit => Self::EagerInit,
            ProviderInitBoundaryKind::FrameworkValidation => Self::FrameworkValidation,
        }
    }
}

#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderRuntimePhase {
    Unknown,
    StartupEagerInit,
    ServiceGenerationResolve,
    ReloadGenerationResolve,
    TriggerDispatchResolve,
    FrameworkValidation,
}

impl ProviderRuntimePhase {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::StartupEagerInit => "startup_eager_init",
            Self::ServiceGenerationResolve => "service_generation_resolve",
            Self::ReloadGenerationResolve => "reload_generation_resolve",
            Self::TriggerDispatchResolve => "trigger_dispatch_resolve",
            Self::FrameworkValidation => "framework_validation",
        }
    }
}

impl From<ProviderRuntimePhase> for ProviderFailureRuntimePhase {
    fn from(value: ProviderRuntimePhase) -> Self {
        match value {
            ProviderRuntimePhase::Unknown => Self::Unknown,
            ProviderRuntimePhase::StartupEagerInit => Self::StartupEagerInit,
            ProviderRuntimePhase::ServiceGenerationResolve => Self::ServiceGenerationResolve,
            ProviderRuntimePhase::ReloadGenerationResolve => Self::ReloadGenerationResolve,
            ProviderRuntimePhase::TriggerDispatchResolve => Self::TriggerDispatchResolve,
            ProviderRuntimePhase::FrameworkValidation => Self::FrameworkValidation,
        }
    }
}

task_local! {
    static CURRENT_PROVIDER_RUNTIME_PHASE: ProviderRuntimePhase;
}

#[doc(hidden)]
pub async fn with_provider_runtime_phase<F, T>(phase: ProviderRuntimePhase, future: F) -> T
where
    F: Future<Output = T>,
{
    CURRENT_PROVIDER_RUNTIME_PHASE.scope(phase, future).await
}

fn current_provider_runtime_phase() -> ProviderRuntimePhase {
    let phase = CURRENT_PROVIDER_RUNTIME_PHASE
        .try_with(|phase| *phase)
        .unwrap_or(ProviderRuntimePhase::Unknown);

    if phase == ProviderRuntimePhase::ServiceGenerationResolve
        && matches!(context::state(), ServiceStatus::NeedReload)
    {
        ProviderRuntimePhase::ReloadGenerationResolve
    } else {
        phase
    }
}

#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderInitSourceKind {
    UserProviderFatal,
    UserProviderRetryableTimeout,
    EnvironmentMissing,
    EnvironmentParse,
    DependencyProvider,
    Panic,
    Cancelled,
    Timeout,
    FrameworkGraphValidation,
    FrameworkEagerInit,
    SystemIoFatal,
    SystemIoRetryable,
    Unknown,
}

impl ProviderInitSourceKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::UserProviderFatal => "user_provider_fatal",
            Self::UserProviderRetryableTimeout => "user_provider_retryable_timeout",
            Self::EnvironmentMissing => "environment_missing",
            Self::EnvironmentParse => "environment_parse",
            Self::DependencyProvider => "dependency_provider",
            Self::Panic => "panic",
            Self::Cancelled => "cancelled",
            Self::Timeout => "timeout",
            Self::FrameworkGraphValidation => "framework_graph_validation",
            Self::FrameworkEagerInit => "framework_eager_init",
            Self::SystemIoFatal => "system_io_fatal",
            Self::SystemIoRetryable => "system_io_retryable",
            Self::Unknown => "unknown",
        }
    }
}

impl From<ProviderInitSourceKind> for ProviderFailureSourceKind {
    fn from(value: ProviderInitSourceKind) -> Self {
        match value {
            ProviderInitSourceKind::UserProviderFatal => Self::UserProviderFatal,
            ProviderInitSourceKind::UserProviderRetryableTimeout => {
                Self::UserProviderRetryableTimeout
            }
            ProviderInitSourceKind::EnvironmentMissing => Self::EnvironmentMissing,
            ProviderInitSourceKind::EnvironmentParse => Self::EnvironmentParse,
            ProviderInitSourceKind::DependencyProvider => Self::DependencyProvider,
            ProviderInitSourceKind::Panic => Self::Panic,
            ProviderInitSourceKind::Cancelled => Self::Cancelled,
            ProviderInitSourceKind::Timeout => Self::Timeout,
            ProviderInitSourceKind::FrameworkGraphValidation => Self::FrameworkGraphValidation,
            ProviderInitSourceKind::FrameworkEagerInit => Self::FrameworkEagerInit,
            ProviderInitSourceKind::SystemIoFatal => Self::SystemIoFatal,
            ProviderInitSourceKind::SystemIoRetryable => Self::SystemIoRetryable,
            ProviderInitSourceKind::Unknown => Self::Unknown,
        }
    }
}

#[doc(hidden)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderInitFailure {
    source: ProviderInitSourceKind,
    error: ProviderInitError,
    retry_diagnostics: Option<ProviderInitRetryDiagnostics>,
}

impl ProviderInitFailure {
    pub fn new(source: ProviderInitSourceKind, error: ProviderInitError) -> Self {
        Self {
            source,
            error,
            retry_diagnostics: None,
        }
    }

    pub fn fatal(provider: &'static str, message: String, source: ProviderInitSourceKind) -> Self {
        Self::new(
            source,
            ProviderInitError::Fatal {
                provider: provider.to_owned(),
                message,
            },
        )
    }

    pub fn timeout(
        provider: &'static str,
        timeout: Duration,
        last_error: String,
        source: ProviderInitSourceKind,
    ) -> Self {
        Self::new(
            source,
            ProviderInitError::Timeout {
                provider: provider.to_owned(),
                timeout,
                last_error,
            },
        )
    }

    fn timeout_with_retry_diagnostics(
        provider: &'static str,
        timeout: Duration,
        last_error: String,
        source: ProviderInitSourceKind,
        retry_diagnostics: ProviderInitRetryDiagnostics,
    ) -> Self {
        Self {
            source,
            error: ProviderInitError::Timeout {
                provider: provider.to_owned(),
                timeout,
                last_error,
            },
            retry_diagnostics: Some(retry_diagnostics),
        }
    }

    pub fn cancelled(provider: &'static str) -> Self {
        Self::new(
            ProviderInitSourceKind::Cancelled,
            ProviderInitError::Cancelled {
                provider: provider.to_owned(),
            },
        )
    }

    pub const fn source(&self) -> ProviderInitSourceKind {
        self.source
    }

    pub const fn error(&self) -> &ProviderInitError {
        &self.error
    }

    pub const fn retry_diagnostics(&self) -> Option<&ProviderInitRetryDiagnostics> {
        self.retry_diagnostics.as_ref()
    }

    pub fn into_error(self) -> ProviderInitError {
        self.error
    }
}

impl From<ProviderInitError> for ProviderInitFailure {
    fn from(error: ProviderInitError) -> Self {
        Self::new(ProviderInitSourceKind::Unknown, error)
    }
}

#[doc(hidden)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderInitRetryDiagnostics {
    attempts: u32,
    elapsed: Duration,
    last_delay: Option<Duration>,
    recent_errors: Vec<String>,
}

impl ProviderInitRetryDiagnostics {
    fn new(
        attempts: u32,
        elapsed: Duration,
        last_delay: Option<Duration>,
        recent_errors: Vec<String>,
    ) -> Self {
        Self {
            attempts,
            elapsed,
            last_delay,
            recent_errors,
        }
    }

    pub const fn attempts(&self) -> u32 {
        self.attempts
    }

    pub const fn elapsed(&self) -> Duration {
        self.elapsed
    }

    pub const fn last_delay(&self) -> Option<Duration> {
        self.last_delay
    }

    pub fn recent_errors(&self) -> &[String] {
        &self.recent_errors
    }
}

struct ProviderInitTimeoutFacts<'a> {
    start: Instant,
    attempts: u32,
    last_delay: Option<Duration>,
    recent_errors: &'a VecDeque<String>,
}

#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProviderInitBoundaryContext {
    provider: &'static str,
    phase: ProviderRuntimePhase,
    boundary: ProviderInitBoundaryKind,
}

impl ProviderInitBoundaryContext {
    pub const fn new(provider: &'static str, boundary: ProviderInitBoundaryKind) -> Self {
        Self {
            provider,
            phase: ProviderRuntimePhase::Unknown,
            boundary,
        }
    }

    pub const fn with_phase(
        provider: &'static str,
        phase: ProviderRuntimePhase,
        boundary: ProviderInitBoundaryKind,
    ) -> Self {
        Self {
            provider,
            phase,
            boundary,
        }
    }

    fn with_current_phase(self) -> Self {
        if self.phase == ProviderRuntimePhase::Unknown {
            Self {
                phase: current_provider_runtime_phase(),
                ..self
            }
        } else {
            self
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProviderInitFailureKind {
    Fatal,
    Timeout,
    Cancelled,
}

impl ProviderInitFailureKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Fatal => "fatal",
            Self::Timeout => "timeout",
            Self::Cancelled => "cancelled",
        }
    }
}

impl From<ProviderInitFailureKind> for ProviderFailureKind {
    fn from(value: ProviderInitFailureKind) -> Self {
        match value {
            ProviderInitFailureKind::Fatal => Self::Fatal,
            ProviderInitFailureKind::Timeout => Self::Timeout,
            ProviderInitFailureKind::Cancelled => Self::Cancelled,
        }
    }
}

fn classify_provider_init_error(error: &ProviderInitError) -> ProviderInitFailureKind {
    match error {
        ProviderInitError::Fatal { .. } => ProviderInitFailureKind::Fatal,
        ProviderInitError::Timeout { .. } => ProviderInitFailureKind::Timeout,
        ProviderInitError::Cancelled { .. } => ProviderInitFailureKind::Cancelled,
    }
}

fn provider_init_timeout_source(
    last_retryable_error: &Option<String>,
    retryable_timeout_source: ProviderInitSourceKind,
) -> ProviderInitSourceKind {
    if last_retryable_error.is_some() {
        retryable_timeout_source
    } else {
        ProviderInitSourceKind::Timeout
    }
}

fn trace_provider_init_failure(
    context: ProviderInitBoundaryContext,
    failure: &ProviderInitFailure,
) {
    let context = context.with_current_phase();
    let failure_kind = classify_provider_init_error(failure.error());
    let retry_attempts = failure.retry_diagnostics().map(|d| d.attempts());
    let retry_elapsed_ms = failure
        .retry_diagnostics()
        .map(|d| duration_millis(d.elapsed()));
    let retry_last_delay_ms = failure
        .retry_diagnostics()
        .and_then(|d| d.last_delay().map(duration_millis));
    let retry_recent_errors = failure.retry_diagnostics().map(|d| d.recent_errors());
    debug!(
        provider = context.provider,
        provider_runtime_phase = context.phase.as_str(),
        provider_init_boundary = context.boundary.as_str(),
        provider_init_source_kind = failure.source().as_str(),
        provider_init_failure_kind = failure_kind.as_str(),
        provider_init_retry_attempts = retry_attempts,
        provider_init_retry_elapsed_ms = retry_elapsed_ms,
        provider_init_retry_last_delay_ms = retry_last_delay_ms,
        provider_init_retry_recent_errors = ?retry_recent_errors,
        error = %failure.error(),
        "Provider init boundary classified error"
    );
}

fn provider_failure_snapshot(
    context: ProviderInitBoundaryContext,
    failure: &ProviderInitFailure,
) -> ProviderFailureSnapshot {
    let context = context.with_current_phase();
    ProviderFailureSnapshot {
        provider: context.provider,
        phase: context.phase.into(),
        boundary: context.boundary.into(),
        source: failure.source().into(),
        failure_kind: classify_provider_init_error(failure.error()).into(),
        retry: failure
            .retry_diagnostics()
            .map(provider_failure_retry_diagnostics_snapshot),
        error: failure.error().to_string(),
    }
}

fn provider_failure_retry_diagnostics_snapshot(
    diagnostics: &ProviderInitRetryDiagnostics,
) -> ProviderFailureRetryDiagnosticsSnapshot {
    ProviderFailureRetryDiagnosticsSnapshot {
        attempts: diagnostics.attempts(),
        elapsed_ms: duration_millis(diagnostics.elapsed()),
        last_delay_ms: diagnostics.last_delay().map(duration_millis),
        recent_errors: diagnostics.recent_errors().to_vec(),
    }
}

fn record_provider_failure_diagnostics(failure: ProviderFailureSnapshot) {
    if let Some(diagnostics) = context::current_generation_diagnostics() {
        diagnostics.record_provider_failure(&failure);
    }
    if let Some(diagnostics) = context::current_daemon_diagnostics() {
        diagnostics.record_provider_failure(failure);
    }
}

fn duration_millis(duration: Duration) -> u64 {
    duration.as_millis().min(u128::from(u64::MAX)) as u64
}

#[doc(hidden)]
pub fn provider_init_failure_into_error(
    context: ProviderInitBoundaryContext,
    failure: ProviderInitFailure,
) -> ProviderInitError {
    trace_provider_init_failure(context, &failure);
    record_provider_failure_diagnostics(provider_failure_snapshot(context, &failure));
    failure.into_error()
}

#[doc(hidden)]
pub fn provider_init_failure_boundary<T>(
    context: ProviderInitBoundaryContext,
    result: Result<T, ProviderInitFailure>,
) -> Result<T, ProviderInitError> {
    match result {
        Ok(value) => Ok(value),
        Err(failure) => Err(provider_init_failure_into_error(context, failure)),
    }
}

#[doc(hidden)]
pub fn provider_init_boundary<T>(
    context: ProviderInitBoundaryContext,
    result: Result<T, ProviderInitError>,
) -> Result<T, ProviderInitError> {
    provider_init_failure_boundary(context, result.map_err(ProviderInitFailure::from))
}

/// Initialize a fallible provider with backoff + timeout.
///
/// This helper preserves the existing retry/backoff/timeout semantics but keeps
/// failures inside the daemon startup boundary by returning [`ProviderInitFailure`]
/// instead of panicking the process.
pub async fn init_fallible<T, Init, Fut>(
    provider: &'static str,
    policy: RestartPolicy,
    cancel: CancellationToken,
    init: Init,
) -> Result<Arc<T>, ProviderInitFailure>
where
    T: Send + Sync + 'static,
    Init: FnMut() -> Fut,
    Fut: Future<Output = Result<T, ProviderError>> + Send,
{
    init_fallible_with_source(
        provider,
        policy,
        cancel,
        ProviderInitSourceKind::UserProviderFatal,
        ProviderInitSourceKind::UserProviderRetryableTimeout,
        init,
    )
    .await
}

#[doc(hidden)]
pub async fn init_fallible_with_source<T, Init, Fut>(
    provider: &'static str,
    policy: RestartPolicy,
    cancel: CancellationToken,
    fatal_source: ProviderInitSourceKind,
    retryable_timeout_source: ProviderInitSourceKind,
    mut init: Init,
) -> Result<Arc<T>, ProviderInitFailure>
where
    T: Send + Sync + 'static,
    Init: FnMut() -> Fut,
    Fut: Future<Output = Result<T, ProviderError>> + Send,
{
    let start = Instant::now();
    let deadline = start + policy.provider_init_timeout;
    let mut last_retryable_error: Option<String> = None;
    let mut retry_attempts: u32 = 0;
    let mut last_retry_delay: Option<Duration> = None;
    let mut recent_retryable_errors: VecDeque<String> = VecDeque::new();

    let mut backoff = BackoffController::new(policy);

    loop {
        let now = Instant::now();
        if now >= deadline {
            let source =
                provider_init_timeout_source(&last_retryable_error, retryable_timeout_source);
            return Err(provider_init_timeout_failure(
                provider,
                policy.provider_init_timeout,
                last_retryable_error.clone().unwrap_or_else(|| {
                    "provider initialization attempt did not complete before timeout".to_owned()
                }),
                source,
                ProviderInitTimeoutFacts {
                    start,
                    attempts: retry_attempts,
                    last_delay: last_retry_delay,
                    recent_errors: &recent_retryable_errors,
                },
            ));
        }

        let remaining = deadline.saturating_duration_since(now);
        let init_result = tokio::select! {
            _ = cancel.cancelled() => {
                info!(provider, "Provider init cancelled while attempt was running");
                return Err(ProviderInitFailure::cancelled(provider));
            }
            res = tokio::time::timeout(remaining, init()) => {
                match res {
                    Ok(res) => res,
                    Err(_) => {
                        let source = provider_init_timeout_source(&last_retryable_error, retryable_timeout_source);
                        return Err(provider_init_timeout_failure(
                            provider,
                            policy.provider_init_timeout,
                            last_retryable_error.clone().unwrap_or_else(|| {
                                "provider initialization attempt did not complete before timeout".to_owned()
                            }),
                            source,
                            ProviderInitTimeoutFacts {
                                start,
                                attempts: retry_attempts,
                                last_delay: last_retry_delay,
                                recent_errors: &recent_retryable_errors,
                            },
                        ));
                    }
                }
            }
        };

        match init_result {
            Ok(v) => {
                return Ok(Arc::new(v));
            }
            Err(ProviderError::Fatal(message)) => {
                error!(provider, "Provider init fatal: {message}");
                return Err(ProviderInitFailure::fatal(provider, message, fatal_source));
            }
            Err(ProviderError::Retryable(message)) => {
                retry_attempts = retry_attempts.saturating_add(1);
                last_retryable_error = Some(message.clone());
                push_recent_retryable_error(&mut recent_retryable_errors, message.clone());
                let now = Instant::now();
                if now >= deadline {
                    return Err(provider_init_timeout_failure(
                        provider,
                        policy.provider_init_timeout,
                        message,
                        retryable_timeout_source,
                        ProviderInitTimeoutFacts {
                            start,
                            attempts: retry_attempts,
                            last_delay: last_retry_delay,
                            recent_errors: &recent_retryable_errors,
                        },
                    ));
                }

                warn!(
                    provider,
                    attempt = backoff.attempt_count(),
                    elapsed_ms = now.duration_since(start).as_millis() as u64,
                    "Provider init retryable error: {message}"
                );

                let remaining = deadline.saturating_duration_since(now);
                let sleep_for = std::cmp::min(backoff.current_delay(), remaining);
                last_retry_delay = Some(sleep_for);

                let proceed = wait_or_cancel_or_timeout(sleep_for, &cancel).await;
                if !proceed {
                    info!(provider, "Provider init cancelled during backoff wait");
                    return Err(ProviderInitFailure::cancelled(provider));
                }

                backoff.record_failure();
            }
        }
    }
}

fn provider_init_timeout_failure(
    provider: &'static str,
    timeout: Duration,
    last_error: String,
    source: ProviderInitSourceKind,
    facts: ProviderInitTimeoutFacts<'_>,
) -> ProviderInitFailure {
    ProviderInitFailure::timeout_with_retry_diagnostics(
        provider,
        timeout,
        last_error,
        source,
        ProviderInitRetryDiagnostics::new(
            facts.attempts,
            facts.start.elapsed(),
            facts.last_delay,
            facts.recent_errors.iter().cloned().collect(),
        ),
    )
}

fn push_recent_retryable_error(recent_errors: &mut VecDeque<String>, message: String) {
    if recent_errors.len() == PROVIDER_INIT_RECENT_ERROR_LIMIT {
        recent_errors.pop_front();
    }
    recent_errors.push_back(message);
}

/// Execute eager provider initialization while translating panics into
/// [`ProviderInitError::Fatal`].
pub async fn catch_init_panic<T, Fut>(
    provider: &'static str,
    fut: Fut,
) -> Result<T, ProviderInitError>
where
    Fut: Future<Output = T> + Send,
{
    match AssertUnwindSafe(fut).catch_unwind().await {
        Ok(value) => Ok(value),
        Err(payload) => Err(ProviderInitError::Fatal {
            provider: provider.to_owned(),
            message: panic_payload_to_string(payload),
        }),
    }
}

fn panic_payload_to_string(payload: Box<dyn Any + Send>) -> String {
    if let Some(message) = payload.downcast_ref::<&'static str>() {
        (*message).to_owned()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "provider initialization panicked with a non-string payload".to_owned()
    }
}

async fn wait_or_cancel_or_timeout(dur: Duration, cancel: &CancellationToken) -> bool {
    tokio::select! {
        _ = sleep(dur) => true,
        _ = cancel.cancelled() => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn test_policy(provider_init_timeout: Duration) -> RestartPolicy {
        RestartPolicy {
            initial_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(5),
            multiplier: 2.0,
            reset_after: Duration::from_secs(1),
            jitter_factor: 0.0,
            wave_spawn_timeout: Duration::from_millis(10),
            provider_init_timeout,
            wave_stop_timeout: Duration::from_millis(10),
            trigger_max_retries: None,
        }
    }

    fn expect_failure<T>(result: Result<T, ProviderInitFailure>) -> ProviderInitFailure {
        match result {
            Ok(_) => panic!("expected provider init failure"),
            Err(failure) => failure,
        }
    }

    #[test]
    fn provider_init_error_classification_distinguishes_runtime_shapes() {
        assert_eq!(
            classify_provider_init_error(&ProviderInitError::Fatal {
                provider: "fatal_provider".to_owned(),
                message: "fatal".to_owned(),
            }),
            ProviderInitFailureKind::Fatal
        );
        assert_eq!(
            classify_provider_init_error(&ProviderInitError::Timeout {
                provider: "timeout_provider".to_owned(),
                timeout: Duration::from_millis(20),
                last_error: "retryable".to_owned(),
            }),
            ProviderInitFailureKind::Timeout
        );
        assert_eq!(
            classify_provider_init_error(&ProviderInitError::Cancelled {
                provider: "cancelled_provider".to_owned(),
            }),
            ProviderInitFailureKind::Cancelled
        );
    }

    #[test]
    fn provider_init_context_models_runtime_phase_and_resolve_boundary() {
        let context = ProviderInitBoundaryContext::new(
            "phase_gap_provider",
            ProviderInitBoundaryKind::SnapshotResolve,
        );

        assert_eq!(context.provider, "phase_gap_provider");
        assert_eq!(context.phase, ProviderRuntimePhase::Unknown);
        assert_eq!(context.boundary, ProviderInitBoundaryKind::SnapshotResolve);
        assert_eq!(context.phase.as_str(), "unknown");
        assert_eq!(context.boundary.as_str(), "snapshot_resolve");

        let eager_context = ProviderInitBoundaryContext::with_phase(
            "phase_gap_provider",
            ProviderRuntimePhase::StartupEagerInit,
            ProviderInitBoundaryKind::EagerInit,
        );
        assert_eq!(eager_context.phase, ProviderRuntimePhase::StartupEagerInit);
        assert_eq!(eager_context.phase.as_str(), "startup_eager_init");
        assert_eq!(eager_context.boundary, ProviderInitBoundaryKind::EagerInit);
    }

    #[tokio::test]
    async fn provider_init_context_uses_scoped_runtime_phase_by_default() {
        let context =
            with_provider_runtime_phase(ProviderRuntimePhase::TriggerDispatchResolve, async {
                ProviderInitBoundaryContext::new(
                    "trigger_provider",
                    ProviderInitBoundaryKind::SnapshotResolve,
                )
                .with_current_phase()
            })
            .await;

        assert_eq!(context.provider, "trigger_provider");
        assert_eq!(context.phase, ProviderRuntimePhase::TriggerDispatchResolve);
        assert_eq!(context.boundary, ProviderInitBoundaryKind::SnapshotResolve);
    }

    #[tokio::test]
    async fn service_provider_phase_upgrades_to_reload_when_reload_token_is_cancelled() {
        let reload_token = CancellationToken::new();
        reload_token.cancel();
        let identity = context::ServiceIdentity::new(
            crate::models::ServiceId::new(700),
            "reload_provider_phase",
            CancellationToken::new(),
            reload_token,
        );

        let context =
            context::__run_service_scope(identity, context::DaemonResources::new(), || {
                with_provider_runtime_phase(ProviderRuntimePhase::ServiceGenerationResolve, async {
                    ProviderInitBoundaryContext::new(
                        "reload_provider",
                        ProviderInitBoundaryKind::SnapshotResolve,
                    )
                    .with_current_phase()
                })
            })
            .await;

        assert_eq!(context.provider, "reload_provider");
        assert_eq!(context.phase, ProviderRuntimePhase::ReloadGenerationResolve);
        assert_eq!(context.boundary, ProviderInitBoundaryKind::SnapshotResolve);
    }

    #[tokio::test]
    async fn reload_provider_failure_projects_public_diagnostics_snapshot() {
        let store = Arc::new(crate::core::diagnostics::DiagnosticsStore::new());
        let service_id = crate::models::ServiceId::new(701);
        let generation_diagnostics = store.register_generation(
            service_id,
            "reload_provider_failure",
            1,
            crate::core::diagnostics::RuntimeLane::Standard,
        );
        let reload_token = CancellationToken::new();
        reload_token.cancel();
        let identity = context::ServiceIdentity::new_with_diagnostics(
            service_id,
            "reload_provider_failure",
            CancellationToken::new(),
            reload_token,
            generation_diagnostics,
        );
        let resources = context::DaemonResources::new_with_diagnostics(store.clone());

        context::__run_service_scope(identity, resources, || {
            with_provider_runtime_phase(ProviderRuntimePhase::ServiceGenerationResolve, async {
                let context = ProviderInitBoundaryContext::new(
                    "reload_failure_provider",
                    ProviderInitBoundaryKind::SnapshotResolve,
                );
                let failure = ProviderInitFailure::fatal(
                    "reload_failure_provider",
                    "reload generation provider failure".to_owned(),
                    ProviderInitSourceKind::UserProviderFatal,
                );
                let _ = provider_init_failure_into_error(context, failure);
            })
        })
        .await;

        let snapshot: crate::models::DaemonDiagnosticsSnapshot = store.snapshot().into();
        let generation = snapshot
            .generations
            .iter()
            .find(|generation| generation.service_id == service_id)
            .expect("generation diagnostics should be projected");
        let failure = generation
            .aggregate
            .provider_failure
            .last_failure
            .as_ref()
            .expect("reload provider failure should be projected");
        assert_eq!(
            failure.phase,
            crate::models::DiagnosticProviderFailureRuntimePhase::ReloadGenerationResolve
        );
        assert_eq!(
            failure.boundary,
            crate::models::DiagnosticProviderFailureBoundaryKind::SnapshotResolve
        );
        assert_eq!(
            failure.source,
            crate::models::DiagnosticProviderFailureSourceKind::UserProviderFatal
        );
    }

    #[test]
    fn provider_init_source_kind_strings_are_stable() {
        assert_eq!(ProviderRuntimePhase::Unknown.as_str(), "unknown");
        assert_eq!(
            ProviderRuntimePhase::StartupEagerInit.as_str(),
            "startup_eager_init"
        );
        assert_eq!(
            ProviderRuntimePhase::ServiceGenerationResolve.as_str(),
            "service_generation_resolve"
        );
        assert_eq!(
            ProviderRuntimePhase::ReloadGenerationResolve.as_str(),
            "reload_generation_resolve"
        );
        assert_eq!(
            ProviderRuntimePhase::TriggerDispatchResolve.as_str(),
            "trigger_dispatch_resolve"
        );
        assert_eq!(
            ProviderRuntimePhase::FrameworkValidation.as_str(),
            "framework_validation"
        );
        assert_eq!(
            ProviderInitSourceKind::UserProviderFatal.as_str(),
            "user_provider_fatal"
        );
        assert_eq!(
            ProviderInitSourceKind::UserProviderRetryableTimeout.as_str(),
            "user_provider_retryable_timeout"
        );
        assert_eq!(
            ProviderInitSourceKind::EnvironmentMissing.as_str(),
            "environment_missing"
        );
        assert_eq!(
            ProviderInitSourceKind::EnvironmentParse.as_str(),
            "environment_parse"
        );
        assert_eq!(
            ProviderInitSourceKind::DependencyProvider.as_str(),
            "dependency_provider"
        );
        assert_eq!(ProviderInitSourceKind::Panic.as_str(), "panic");
        assert_eq!(ProviderInitSourceKind::Cancelled.as_str(), "cancelled");
        assert_eq!(ProviderInitSourceKind::Timeout.as_str(), "timeout");
        assert_eq!(
            ProviderInitSourceKind::FrameworkGraphValidation.as_str(),
            "framework_graph_validation"
        );
        assert_eq!(
            ProviderInitSourceKind::FrameworkEagerInit.as_str(),
            "framework_eager_init"
        );
        assert_eq!(
            ProviderInitSourceKind::SystemIoFatal.as_str(),
            "system_io_fatal"
        );
        assert_eq!(
            ProviderInitSourceKind::SystemIoRetryable.as_str(),
            "system_io_retryable"
        );
        assert_eq!(ProviderInitSourceKind::Unknown.as_str(), "unknown");
    }

    #[test]
    fn provider_init_boundary_preserves_result_shape() {
        let context = ProviderInitBoundaryContext::new(
            "boundary_provider",
            ProviderInitBoundaryKind::SnapshotResolve,
        );
        assert_eq!(provider_init_boundary(context, Ok::<u32, _>(7)), Ok(7));

        let fatal = ProviderInitError::Fatal {
            provider: "boundary_provider".to_owned(),
            message: "fatal".to_owned(),
        };
        assert_eq!(
            provider_init_boundary::<u32>(context, Err(fatal.clone())),
            Err(fatal)
        );
    }

    #[test]
    fn provider_init_failure_boundary_preserves_source_until_translation() {
        let context = ProviderInitBoundaryContext::new(
            "boundary_provider",
            ProviderInitBoundaryKind::SnapshotResolve,
        );
        let failure = ProviderInitFailure::fatal(
            "boundary_provider",
            "fatal".to_owned(),
            ProviderInitSourceKind::EnvironmentParse,
        );
        assert_eq!(failure.source(), ProviderInitSourceKind::EnvironmentParse);

        assert_eq!(
            provider_init_failure_boundary::<u32>(context, Err(failure.clone())),
            Err(failure.into_error())
        );
    }

    #[test]
    fn provider_init_retry_diagnostics_keep_recent_errors_bounded() {
        let mut recent_errors = VecDeque::new();
        for index in 0..6 {
            push_recent_retryable_error(&mut recent_errors, format!("retry-{index}"));
        }

        let failure = provider_init_timeout_failure(
            "bounded_retry_provider",
            Duration::from_millis(50),
            "retry-5".to_owned(),
            ProviderInitSourceKind::UserProviderRetryableTimeout,
            ProviderInitTimeoutFacts {
                start: Instant::now(),
                attempts: 6,
                last_delay: Some(Duration::from_millis(8)),
                recent_errors: &recent_errors,
            },
        );
        let diagnostics = failure
            .retry_diagnostics()
            .expect("retry timeout should carry diagnostics");

        assert_eq!(diagnostics.attempts(), 6);
        assert_eq!(diagnostics.last_delay(), Some(Duration::from_millis(8)));
        assert_eq!(
            diagnostics.recent_errors(),
            &[
                "retry-2".to_owned(),
                "retry-3".to_owned(),
                "retry-4".to_owned(),
                "retry-5".to_owned(),
            ]
        );
    }

    #[tokio::test]
    async fn init_fallible_retries_then_succeeds() {
        let policy = test_policy(Duration::from_millis(50));
        let cancel = CancellationToken::new();
        let attempts = Arc::new(AtomicU32::new(0));
        let attempts2 = attempts.clone();

        let v = init_fallible("test_provider", policy, cancel, move || {
            let attempts = attempts2.clone();
            async move {
                let n = attempts.fetch_add(1, Ordering::SeqCst);
                if n < 2 {
                    Err(ProviderError::Retryable("not yet".to_owned()))
                } else {
                    Ok(42u32)
                }
            }
        })
        .await
        .unwrap_or_else(|err| panic!("unexpected init failure: {:?}", err));

        assert_eq!(*v, 42);
        assert!(attempts.load(Ordering::SeqCst) >= 3);
    }

    #[tokio::test]
    async fn init_fallible_returns_timeout_error() {
        let policy = test_policy(Duration::from_millis(20));
        let cancel = CancellationToken::new();

        let failure = expect_failure(
            init_fallible::<u32, _, _>("timeout_provider", policy, cancel, || async {
                Err::<u32, _>(ProviderError::Retryable("still broken".to_owned()))
            })
            .await,
        );

        assert_eq!(
            failure.source(),
            ProviderInitSourceKind::UserProviderRetryableTimeout
        );
        let diagnostics = failure
            .retry_diagnostics()
            .expect("retryable timeout should carry retry diagnostics");
        assert!(diagnostics.attempts() > 0);
        assert!(diagnostics.elapsed() <= Duration::from_secs(1));
        assert!(diagnostics.last_delay().is_some());
        assert!(
            diagnostics
                .recent_errors()
                .iter()
                .all(|error| error == "still broken")
        );
        assert_eq!(
            failure.into_error(),
            ProviderInitError::Timeout {
                provider: "timeout_provider".to_owned(),
                timeout: Duration::from_millis(20),
                last_error: "still broken".to_owned(),
            }
        );
    }

    #[tokio::test]
    async fn init_fallible_returns_fatal_error() {
        let policy = test_policy(Duration::from_millis(50));
        let cancel = CancellationToken::new();

        let failure = expect_failure(
            init_fallible::<u32, _, _>("fatal_provider", policy, cancel, || async {
                Err::<u32, _>(ProviderError::Fatal("bad config".to_owned()))
            })
            .await,
        );

        assert_eq!(failure.source(), ProviderInitSourceKind::UserProviderFatal);
        assert_eq!(
            failure.into_error(),
            ProviderInitError::Fatal {
                provider: "fatal_provider".to_owned(),
                message: "bad config".to_owned(),
            }
        );
    }

    #[tokio::test]
    async fn init_fallible_returns_cancelled_error() {
        let policy = test_policy(Duration::from_secs(1));
        let cancel = CancellationToken::new();
        cancel.cancel();

        let failure = expect_failure(
            init_fallible::<u32, _, _>("cancel_provider", policy, cancel, || async {
                Err::<u32, _>(ProviderError::Retryable("retry later".to_owned()))
            })
            .await,
        );

        assert_eq!(failure.source(), ProviderInitSourceKind::Cancelled);
        assert_eq!(
            failure.into_error(),
            ProviderInitError::Cancelled {
                provider: "cancel_provider".to_owned(),
            }
        );
    }

    #[tokio::test]
    async fn init_fallible_returns_cancelled_error_while_attempt_is_running() {
        let policy = test_policy(Duration::from_secs(1));
        let cancel = CancellationToken::new();
        let cancel_for_task = cancel.clone();

        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            cancel_for_task.cancel();
        });

        let failure = expect_failure(
            init_fallible::<u32, _, _>("cancel_running_provider", policy, cancel, || async {
                tokio::time::sleep(Duration::from_secs(30)).await;
                Ok::<u32, ProviderError>(42)
            })
            .await,
        );

        assert_eq!(failure.source(), ProviderInitSourceKind::Cancelled);
        assert_eq!(
            failure.into_error(),
            ProviderInitError::Cancelled {
                provider: "cancel_running_provider".to_owned(),
            }
        );
    }

    #[tokio::test]
    async fn init_fallible_returns_timeout_error_while_attempt_is_running() {
        let policy = test_policy(Duration::from_millis(20));
        let cancel = CancellationToken::new();

        let failure = expect_failure(
            init_fallible::<u32, _, _>("timeout_running_provider", policy, cancel, || async {
                tokio::time::sleep(Duration::from_secs(30)).await;
                Ok::<u32, ProviderError>(42)
            })
            .await,
        );

        assert_eq!(failure.source(), ProviderInitSourceKind::Timeout);
        let diagnostics = failure
            .retry_diagnostics()
            .expect("running attempt timeout should carry retry diagnostics");
        assert_eq!(diagnostics.attempts(), 0);
        assert_eq!(diagnostics.last_delay(), None);
        assert!(diagnostics.recent_errors().is_empty());
        assert_eq!(
            failure.into_error(),
            ProviderInitError::Timeout {
                provider: "timeout_running_provider".to_owned(),
                timeout: Duration::from_millis(20),
                last_error: "provider initialization attempt did not complete before timeout"
                    .to_owned(),
            }
        );
    }

    #[tokio::test]
    async fn catch_init_panic_returns_successful_value() {
        let result = catch_init_panic("ok_provider", async { 42u32 }).await;

        assert_eq!(result, Ok(42));
    }

    #[tokio::test]
    async fn catch_init_panic_maps_string_payload_to_fatal_error() {
        let result = catch_init_panic::<u32, _>("panic_provider", async {
            panic!("invalid provider configuration")
        })
        .await;

        assert_eq!(
            result,
            Err(ProviderInitError::Fatal {
                provider: "panic_provider".to_owned(),
                message: "invalid provider configuration".to_owned(),
            })
        );
    }

    #[tokio::test]
    async fn catch_init_panic_maps_non_string_payload_to_fatal_error() {
        let result =
            catch_init_panic::<u32, _>("panic_provider", async { std::panic::panic_any(42u32) })
                .await;

        assert_eq!(
            result,
            Err(ProviderInitError::Fatal {
                provider: "panic_provider".to_owned(),
                message: "provider initialization panicked with a non-string payload".to_owned(),
            })
        );
    }

    #[tokio::test]
    async fn catch_init_panic_failure_can_be_tagged_as_panic_source() {
        let context = ProviderInitBoundaryContext::new(
            "panic_provider",
            ProviderInitBoundaryKind::SnapshotResolve,
        );
        let error = match catch_init_panic::<u32, _>("panic_provider", async {
            panic!("invalid provider configuration")
        })
        .await
        {
            Ok(_) => panic!("expected panic error"),
            Err(error) => error,
        };
        let failure = ProviderInitFailure::new(ProviderInitSourceKind::Panic, error);

        assert_eq!(failure.source(), ProviderInitSourceKind::Panic);
        assert!(matches!(
            provider_init_failure_boundary::<u32>(context, Err(failure)),
            Err(ProviderInitError::Fatal { provider, .. }) if provider == "panic_provider"
        ));
    }
}
