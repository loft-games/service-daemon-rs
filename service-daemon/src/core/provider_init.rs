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
}

impl ProviderInitBoundaryKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::SnapshotResolve => "snapshot_resolve",
            Self::RwLockResolve => "rwlock_resolve",
            Self::MutexResolve => "mutex_resolve",
            Self::EagerInit => "eager_init",
        }
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

#[doc(hidden)]
pub fn provider_init_boundary<T>(
    context: ProviderInitBoundaryContext,
    result: Result<T, ProviderInitError>,
) -> Result<T, ProviderInitError> {
    if let Err(error) = &result {
        let failure_kind = classify_provider_init_error(error);
        debug!(
            provider = context.provider,
            provider_init_boundary = context.boundary.as_str(),
            provider_init_failure_kind = failure_kind.as_str(),
            error = %error,
            "Provider init boundary classified error"
        );
    }

    result
}

/// Initialize a fallible provider with backoff + timeout.
///
/// This helper preserves the existing retry/backoff/timeout semantics but keeps
/// failures inside the daemon startup boundary by returning [`ProviderInitError`]
/// instead of panicking the process.
pub async fn init_fallible<T, Init, Fut>(
    provider: &'static str,
    policy: RestartPolicy,
    cancel: CancellationToken,
    mut init: Init,
) -> Result<Arc<T>, ProviderInitError>
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
            return Err(ProviderInitError::Timeout {
                provider: provider.to_owned(),
                timeout: policy.provider_init_timeout,
                last_error: last_retryable_error.clone().unwrap_or_else(|| {
                    "provider initialization attempt did not complete before timeout".to_owned()
                }),
            });
        }

        let remaining = deadline.saturating_duration_since(now);
        let init_result = tokio::select! {
            _ = cancel.cancelled() => {
                info!(provider, "Provider init cancelled while attempt was running");
                return Err(ProviderInitError::Cancelled {
                    provider: provider.to_owned(),
                });
            }
            res = tokio::time::timeout(remaining, init()) => {
                match res {
                    Ok(res) => res,
                    Err(_) => {
                        return Err(ProviderInitError::Timeout {
                            provider: provider.to_owned(),
                            timeout: policy.provider_init_timeout,
                            last_error: last_retryable_error.clone().unwrap_or_else(|| {
                                "provider initialization attempt did not complete before timeout".to_owned()
                            }),
                        });
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
                return Err(ProviderInitError::Fatal {
                    provider: provider.to_owned(),
                    message,
                });
            }
            Err(ProviderError::Retryable(message)) => {
                last_retryable_error = Some(message.clone());
                let now = Instant::now();
                if now >= deadline {
                    return Err(ProviderInitError::Timeout {
                        provider: provider.to_owned(),
                        timeout: policy.provider_init_timeout,
                        last_error: message,
                    });
                }

                warn!(
                    provider,
                    attempt = backoff.attempt_count(),
                    elapsed_ms = now.duration_since(start).as_millis() as u64,
                    "Provider init retryable error: {message}"
                );

                // Wait for the backoff delay (or cancellation), but also enforce
                // the overall init timeout. We cap each sleep to the remaining time.
                let remaining = deadline.saturating_duration_since(now);
                let sleep_for = std::cmp::min(backoff.current_delay(), remaining);

                let proceed = wait_or_cancel_or_timeout(sleep_for, &cancel).await;
                if !proceed {
                    info!(provider, "Provider init cancelled during backoff wait");
                    return Err(ProviderInitError::Cancelled {
                        provider: provider.to_owned(),
                    });
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
        .unwrap_or_else(|err| panic!("unexpected init failure: {err}"));

        assert_eq!(*v, 42);
        assert!(attempts.load(Ordering::SeqCst) >= 3);
    }

    #[tokio::test]
    async fn init_fallible_returns_timeout_error() {
        let policy = test_policy(Duration::from_millis(20));
        let cancel = CancellationToken::new();

        let result = init_fallible::<u32, _, _>("timeout_provider", policy, cancel, || async {
            Err::<u32, _>(ProviderError::Retryable("still broken".to_owned()))
        })
        .await;

        assert_eq!(
            result,
            Err(ProviderInitError::Timeout {
                provider: "timeout_provider".to_owned(),
                timeout: Duration::from_millis(20),
                last_error: "still broken".to_owned(),
            })
        );
    }

    #[tokio::test]
    async fn init_fallible_returns_fatal_error() {
        let policy = test_policy(Duration::from_millis(50));
        let cancel = CancellationToken::new();

        let result = init_fallible::<u32, _, _>("fatal_provider", policy, cancel, || async {
            Err::<u32, _>(ProviderError::Fatal("bad config".to_owned()))
        })
        .await;

        assert_eq!(
            result,
            Err(ProviderInitError::Fatal {
                provider: "fatal_provider".to_owned(),
                message: "bad config".to_owned(),
            })
        );
    }

    #[tokio::test]
    async fn init_fallible_returns_cancelled_error() {
        let policy = test_policy(Duration::from_secs(1));
        let cancel = CancellationToken::new();
        cancel.cancel();

        let result = init_fallible::<u32, _, _>("cancel_provider", policy, cancel, || async {
            Err::<u32, _>(ProviderError::Retryable("retry later".to_owned()))
        })
        .await;

        assert_eq!(
            result,
            Err(ProviderInitError::Cancelled {
                provider: "cancel_provider".to_owned(),
            })
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

        let result =
            init_fallible::<u32, _, _>("cancel_running_provider", policy, cancel, || async {
                tokio::time::sleep(Duration::from_secs(30)).await;
                Ok::<u32, ProviderError>(42)
            })
            .await;

        assert_eq!(
            result,
            Err(ProviderInitError::Cancelled {
                provider: "cancel_running_provider".to_owned(),
            })
        );
    }

    #[tokio::test]
    async fn init_fallible_returns_timeout_error_while_attempt_is_running() {
        let policy = test_policy(Duration::from_millis(20));
        let cancel = CancellationToken::new();

        let result =
            init_fallible::<u32, _, _>("timeout_running_provider", policy, cancel, || async {
                tokio::time::sleep(Duration::from_secs(30)).await;
                Ok::<u32, ProviderError>(42)
            })
            .await;

        assert_eq!(
            result,
            Err(ProviderInitError::Timeout {
                provider: "timeout_running_provider".to_owned(),
                timeout: Duration::from_millis(20),
                last_error: "provider initialization attempt did not complete before timeout"
                    .to_owned(),
            })
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
}
