use crate::models::{BackoffController, ProviderError, ProviderInitError, RestartPolicy};
use futures::FutureExt;
use std::any::Any;
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

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

#[doc(hidden)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderInitFailure {
    source: ProviderInitSourceKind,
    error: ProviderInitError,
}

impl ProviderInitFailure {
    pub fn new(source: ProviderInitSourceKind, error: ProviderInitError) -> Self {
        Self { source, error }
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProviderInitBoundaryContext {
    provider: &'static str,
    boundary: ProviderInitBoundaryKind,
}

impl ProviderInitBoundaryContext {
    pub const fn new(provider: &'static str, boundary: ProviderInitBoundaryKind) -> Self {
        Self { provider, boundary }
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
    let failure_kind = classify_provider_init_error(failure.error());
    debug!(
        provider = context.provider,
        provider_init_boundary = context.boundary.as_str(),
        provider_init_source_kind = failure.source().as_str(),
        provider_init_failure_kind = failure_kind.as_str(),
        error = %failure.error(),
        "Provider init boundary classified error"
    );
}

#[doc(hidden)]
pub fn provider_init_failure_into_error(
    context: ProviderInitBoundaryContext,
    failure: ProviderInitFailure,
) -> ProviderInitError {
    trace_provider_init_failure(context, &failure);
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

    let mut backoff = BackoffController::new(policy);

    loop {
        let now = Instant::now();
        if now >= deadline {
            let source =
                provider_init_timeout_source(&last_retryable_error, retryable_timeout_source);
            return Err(ProviderInitFailure::timeout(
                provider,
                policy.provider_init_timeout,
                last_retryable_error.clone().unwrap_or_else(|| {
                    "provider initialization attempt did not complete before timeout".to_owned()
                }),
                source,
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
                        return Err(ProviderInitFailure::timeout(
                            provider,
                            policy.provider_init_timeout,
                            last_retryable_error.clone().unwrap_or_else(|| {
                                "provider initialization attempt did not complete before timeout".to_owned()
                            }),
                            source,
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
                last_retryable_error = Some(message.clone());
                let now = Instant::now();
                if now >= deadline {
                    return Err(ProviderInitFailure::timeout(
                        provider,
                        policy.provider_init_timeout,
                        message,
                        retryable_timeout_source,
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
    fn provider_init_source_kind_strings_are_stable() {
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
