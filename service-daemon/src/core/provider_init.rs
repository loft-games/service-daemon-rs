use crate::models::{BackoffController, ProviderError, ProviderInitError, RestartPolicy};
use futures::FutureExt;
use std::any::Any;
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

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

    let mut backoff = BackoffController::new(policy);

    loop {
        match init().await {
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
}
