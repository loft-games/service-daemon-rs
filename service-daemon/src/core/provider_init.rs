use crate::models::{BackoffController, ProviderError, RestartPolicy};
use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

#[cfg(test)]
fn provider_init_exit(code: i32, msg: &str) -> ! {
    panic!("provider_init_exit({code}): {msg}");
}

#[cfg(not(test))]
fn provider_init_exit(code: i32, msg: &str) -> ! {
    eprintln!("{msg}");
    std::process::exit(code);
}

/// Initialize a fallible provider with backoff + timeout.
///
/// This helper does **not** return errors to user code.
/// On fatal errors or timeout, it terminates the process.
pub async fn init_fallible<T, Init, Fut>(
    policy: RestartPolicy,
    cancel: CancellationToken,
    init: Init,
) -> Arc<T>
where
    T: Send + Sync + 'static,
    Init: Fn() -> Fut,
    Fut: Future<Output = Result<T, ProviderError>> + Send,
{
    let start = Instant::now();
    let deadline = start + policy.provider_init_timeout;

    let mut backoff = BackoffController::new(policy);

    loop {
        match init().await {
            Ok(v) => {
                return Arc::new(v);
            }
            Err(ProviderError::Fatal(msg)) => {
                error!("Provider init fatal: {msg}");
                provider_init_exit(1, &format!("FATAL: provider init failed: {msg}"));
            }
            Err(ProviderError::Retryable(msg)) => {
                let now = Instant::now();
                if now >= deadline {
                    provider_init_exit(
                        1,
                        &format!(
                            "FATAL: provider init timed out after {:?}: {msg}",
                            policy.provider_init_timeout
                        ),
                    );
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
                    provider_init_exit(1, "FATAL: provider init cancelled");
                }

                backoff.record_failure();
            }
        }
    }
}

async fn wait_or_cancel_or_timeout(dur: Duration, cancel: &CancellationToken) -> bool {
    tokio::select! {
        _ = tokio::time::sleep(dur) => true,
        _ = cancel.cancelled() => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let attempts = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let attempts2 = attempts.clone();

        let v = init_fallible(policy, cancel, move || {
            let attempts = attempts2.clone();
            async move {
                let n = attempts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if n < 2 {
                    Err(ProviderError::Retryable("not yet".to_owned()))
                } else {
                    Ok(42u32)
                }
            }
        })
        .await;

        assert_eq!(*v, 42);
        assert!(attempts.load(std::sync::atomic::Ordering::SeqCst) >= 3);
    }

    #[tokio::test]
    async fn init_fallible_times_out_panics_in_tests() {
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

        let handle = tokio::spawn(async move {
            let _ = init_fallible::<u32, _, _>(policy, cancel, || async {
                Err::<u32, _>(ProviderError::Retryable("still broken".to_owned()))
            })
            .await;
        });

        let res = handle.await;
        assert!(res.is_err(), "expected JoinError due to panic");
        let err = res.unwrap_err();
        assert!(err.is_panic());
    }
}
