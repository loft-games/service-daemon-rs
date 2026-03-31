use crate::models::{BackoffController, ProviderError, RestartPolicy};
use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

/// Panic with the provided message to signal a fatal provider initialization failure.
///
/// This is used internally by the orchestration engine to ensure the current
/// initialization task fails fast when an unrecoverable error (e.g., config error) occurs.
/// Initialize a fallible provider with backoff + timeout.
///
/// Returns `Err(ProviderError::Fatal)` on unrecoverable errors, allowing the
/// caller (usually a macro-generated wrapper) to decide how to handle it.
pub async fn init_fallible<T, Init, Fut>(
    policy: RestartPolicy,
    cancel: CancellationToken,
    mut init: Init,
) -> Result<Arc<T>, ProviderError>
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
            Err(ProviderError::Fatal(msg)) => {
                error!("Provider init fatal: {msg}");
                return Err(ProviderError::Fatal(msg));
            }
            Err(ProviderError::Retryable(msg)) => {
                let now = Instant::now();
                if now >= deadline {
                    return Err(ProviderError::Fatal(format!(
                        "provider init timed out after {:?}: {}",
                        policy.provider_init_timeout, msg
                    )));
                }

                warn!(
                    attempt = backoff.attempt_count(),
                    elapsed_ms = now.duration_since(start).as_millis() as u64,
                    "Provider init retryable error: {msg}"
                );

                // Wait for the backoff delay (or cancellation), but also enforce
                // the overall init timeout. We cap each sleep to the remaining time.
                let remaining = deadline.saturating_duration_since(now);
                let sleep_for = std::cmp::min(backoff.current_delay(), remaining);

                let proceed = wait_or_cancel_or_timeout(sleep_for, &cancel).await;
                if !proceed {
                    info!("Provider init cancelled during backoff wait");
                    return Err(ProviderError::Fatal("provider init cancelled".into()));
                }

                backoff.record_failure();
            }
        }
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

    #[tokio::test]
    async fn init_fallible_retries_then_succeeds() {
        let policy = RestartPolicy {
            initial_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(5),
            multiplier: 2.0,
            reset_after: Duration::from_secs(1),
            jitter_factor: 0.0,
            wave_spawn_timeout: Duration::from_millis(10),
            provider_init_timeout: Duration::from_millis(50),
            wave_stop_timeout: Duration::from_millis(10),
            trigger_max_retries: None,
        };

        let cancel = CancellationToken::new();
        let attempts = Arc::new(AtomicU32::new(0));
        let attempts2 = attempts.clone();

        let v = init_fallible(policy, cancel, move || {
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
        .await;

        assert_eq!(*v.unwrap(), 42);
        assert!(attempts.load(Ordering::SeqCst) >= 3);
    }

    #[tokio::test]
    async fn init_fallible_times_out_returns_fatal() {
        let policy = RestartPolicy {
            initial_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(5),
            multiplier: 2.0,
            reset_after: Duration::from_secs(1),
            jitter_factor: 0.0,
            wave_spawn_timeout: Duration::from_millis(10),
            provider_init_timeout: Duration::from_millis(20),
            wave_stop_timeout: Duration::from_millis(10),
            trigger_max_retries: None,
        };

        let cancel = CancellationToken::new();

        let res = init_fallible::<u32, _, _>(policy, cancel, || async {
            Err::<u32, _>(ProviderError::Retryable("still broken".to_owned()))
        })
        .await;

        assert!(res.is_err());
        let err = res.unwrap_err();
        match err {
            ProviderError::Fatal(msg) => {
                assert!(msg.contains("timed out"));
            }
            _ => panic!("expected Fatal error on timeout, got {:?}", err),
        }
    }
}
