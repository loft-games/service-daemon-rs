//! Shared retry and backoff policies used across services and triggers.
//!
//! This module provides two complementary types:
//!
//! - [`RestartPolicy`]: A configuration object describing backoff parameters
//!   (initial delay, max delay, jitter, etc.). It is **stateless** and can be
//!   shared across many supervisors.
//! - [`BackoffController`]: A **stateful** controller that tracks the current
//!   backoff delay and attempt count. It wraps a `RestartPolicy` and provides
//!   interruption-aware waiting via `tokio::select!`.
//!
//! Both `ServiceSupervisor` (for long-running services) and `TriggerInvocation`
//! (for individual trigger event retries) compose `BackoffController` to share
//! identical retry semantics.

use rand::Rng;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use tracing::info;

// ---------------------------------------------------------------------------
// RestartPolicy -- stateless backoff configuration
// ---------------------------------------------------------------------------

/// Configuration for retry / restart behavior with exponential backoff.
///
/// This struct is **stateless** -- it only describes *how* to compute delays.
/// Pair it with [`BackoffController`] to get a stateful retry loop.
#[derive(Debug, Clone, Copy)]
pub struct RestartPolicy {
    /// Initial delay before the first retry (default: 1 second).
    pub initial_delay: Duration,
    /// Maximum delay between retries (default: 5 minutes).
    pub max_delay: Duration,
    /// Multiplier for exponential backoff (default: 2.0).
    pub multiplier: f64,
    /// Delay resets to `initial_delay` after this duration of successful
    /// running (default: 60 seconds).
    pub reset_after: Duration,
    /// Jitter factor (0.0-1.0) -- randomises delay to prevent thundering
    /// herd (default: 0.1).
    pub jitter_factor: f64,
    /// Timeout for waiting for services to become healthy during wave
    /// startup (default: 5 seconds).
    pub wave_spawn_timeout: Duration,
    /// Timeout for waiting for services to stop during wave shutdown
    /// (default: 30 seconds).
    pub wave_stop_timeout: Duration,
}

impl Default for RestartPolicy {
    fn default() -> Self {
        Self {
            initial_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(300), // 5 minutes
            multiplier: 2.0,
            reset_after: Duration::from_secs(60),
            jitter_factor: 0.1, // 10% jitter by default
            wave_spawn_timeout: Duration::from_secs(5),
            wave_stop_timeout: Duration::from_secs(30),
        }
    }
}

impl RestartPolicy {
    /// Create a restart policy builder.
    pub fn builder() -> RestartPolicyBuilder {
        RestartPolicyBuilder::default()
    }

    /// Create a restart policy for testing with shorter delays.
    pub fn for_testing() -> Self {
        Self {
            initial_delay: Duration::from_millis(100),
            max_delay: Duration::from_secs(2),
            multiplier: 2.0,
            reset_after: Duration::from_secs(5),
            jitter_factor: 0.0, // No jitter for predictable tests
            wave_spawn_timeout: Duration::from_millis(500),
            wave_stop_timeout: Duration::from_secs(2),
        }
    }

    /// Calculate the next retry delay using exponential backoff with jitter.
    pub fn next_delay(&self, current_delay: Duration) -> Duration {
        let base = current_delay.as_secs_f64() * self.multiplier;
        let jitter_range = base * self.jitter_factor;
        let jitter = rand::rng().random_range(-jitter_range..=jitter_range);
        let next = Duration::from_secs_f64((base + jitter).max(0.0));
        next.min(self.max_delay)
    }
}

/// Builder for [`RestartPolicy`].
#[derive(Default)]
pub struct RestartPolicyBuilder {
    policy: RestartPolicy,
}

impl RestartPolicyBuilder {
    pub fn initial_delay(mut self, delay: Duration) -> Self {
        self.policy.initial_delay = delay;
        self
    }

    pub fn max_delay(mut self, delay: Duration) -> Self {
        self.policy.max_delay = delay;
        self
    }

    pub fn multiplier(mut self, multiplier: f64) -> Self {
        self.policy.multiplier = multiplier;
        self
    }

    pub fn reset_after(mut self, duration: Duration) -> Self {
        self.policy.reset_after = duration;
        self
    }

    pub fn jitter_factor(mut self, factor: f64) -> Self {
        self.policy.jitter_factor = factor.clamp(0.0, 1.0);
        self
    }

    pub fn wave_spawn_timeout(mut self, timeout: Duration) -> Self {
        self.policy.wave_spawn_timeout = timeout;
        self
    }

    pub fn wave_stop_timeout(mut self, timeout: Duration) -> Self {
        self.policy.wave_stop_timeout = timeout;
        self
    }

    #[must_use]
    pub fn build(self) -> RestartPolicy {
        self.policy
    }
}

// ---------------------------------------------------------------------------
// BackoffController -- stateful retry engine
// ---------------------------------------------------------------------------

/// A stateful controller that manages exponential-backoff retry loops.
///
/// `BackoffController` encapsulates the mutable state (current delay, attempt
/// counter) required for retries, while delegating the policy parameters to
/// the embedded [`RestartPolicy`].
///
/// It provides **interruption-aware waiting**: when waiting for the next
/// retry, it races the backoff sleep against a [`CancellationToken`] so that
/// shutdown signals are respected immediately.
///
/// # Usage
///
/// ```rust,ignore
/// let mut backoff = BackoffController::new(RestartPolicy::default());
///
/// loop {
///     match do_work().await {
///         Ok(_) => {
///             backoff.record_success();
///             break;
///         }
///         Err(e) => {
///             if !backoff.wait_or_cancel(&cancel_token).await {
///                 break; // shutdown requested
///             }
///         }
///     }
/// }
/// ```
pub struct BackoffController {
    /// The backoff configuration (stateless).
    policy: RestartPolicy,
    /// The current delay that will be applied on the next retry.
    current_delay: Duration,
    /// The number of consecutive failed attempts.
    attempt_count: u32,
}

impl BackoffController {
    /// Create a new controller with the given policy.
    ///
    /// The initial delay is taken from `policy.initial_delay`.
    pub fn new(policy: RestartPolicy) -> Self {
        let initial = policy.initial_delay;
        Self {
            policy,
            current_delay: initial,
            attempt_count: 0,
        }
    }

    /// Returns a reference to the underlying policy.
    #[inline]
    pub fn policy(&self) -> &RestartPolicy {
        &self.policy
    }

    /// Returns the current backoff delay.
    #[inline]
    pub fn current_delay(&self) -> Duration {
        self.current_delay
    }

    /// Returns how many consecutive failures have been recorded.
    #[inline]
    pub fn attempt_count(&self) -> u32 {
        self.attempt_count
    }

    /// Record a successful execution.
    ///
    /// Resets the delay back to `policy.initial_delay` and clears the
    /// attempt counter.
    pub fn record_success(&mut self) {
        self.current_delay = self.policy.initial_delay;
        self.attempt_count = 0;
    }

    /// Record a failed execution and advance the backoff delay.
    ///
    /// The delay is multiplied according to the policy's exponential
    /// backoff parameters.
    pub fn record_failure(&mut self) {
        self.attempt_count += 1;
        self.current_delay = self.policy.next_delay(self.current_delay);
    }

    /// Reset the delay if the elapsed running time exceeds `policy.reset_after`.
    ///
    /// Call this after a service/handler has been running for a while to
    /// indicate that the previous crash pattern has stabilised.
    pub fn maybe_reset(&mut self, elapsed: Duration) {
        if elapsed >= self.policy.reset_after {
            self.record_success();
        }
    }

    /// Sleep for the current backoff delay, but cancel early if the
    /// provided [`CancellationToken`] fires.
    ///
    /// Returns `true` if the sleep completed normally (retry may proceed).
    /// Returns `false` if the token was cancelled (caller should stop).
    pub async fn wait_or_cancel(&self, cancel_token: &CancellationToken) -> bool {
        info!(
            delay_ms = self.current_delay.as_millis() as u64,
            attempt = self.attempt_count,
            "Backoff: waiting before next attempt"
        );
        tokio::select! {
            _ = tokio::time::sleep(self.current_delay) => true,
            _ = cancel_token.cancelled() => {
                info!("Backoff: cancelled during wait");
                false
            }
        }
    }

    /// Convenience method: sleep for the current delay, advance the
    /// backoff, and check cancellation -- all in one call.
    ///
    /// Returns `true` if retry should proceed, `false` if cancelled.
    pub async fn backoff_or_cancel(&mut self, cancel_token: &CancellationToken) -> bool {
        let proceed = self.wait_or_cancel(cancel_token).await;
        if proceed {
            self.current_delay = self.policy.next_delay(self.current_delay);
        }
        proceed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_restart_policy_default() {
        let policy = RestartPolicy::default();
        assert_eq!(policy.initial_delay, Duration::from_secs(1));
        assert_eq!(policy.max_delay, Duration::from_secs(300));
    }

    #[test]
    fn test_restart_policy_next_delay_respects_max() {
        let policy = RestartPolicy {
            initial_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(10),
            multiplier: 100.0,
            jitter_factor: 0.0,
            ..RestartPolicy::default()
        };
        let next = policy.next_delay(Duration::from_secs(1));
        // 1 * 100 = 100, clamped to max_delay = 10
        assert_eq!(next, Duration::from_secs(10));
    }

    #[test]
    fn test_backoff_controller_record_success_resets() {
        let mut ctrl = BackoffController::new(RestartPolicy::for_testing());
        ctrl.record_failure();
        ctrl.record_failure();
        assert!(ctrl.attempt_count() == 2);
        assert!(ctrl.current_delay() > ctrl.policy().initial_delay);

        ctrl.record_success();
        assert_eq!(ctrl.attempt_count(), 0);
        assert_eq!(ctrl.current_delay(), ctrl.policy().initial_delay);
    }

    #[test]
    fn test_backoff_controller_maybe_reset() {
        let mut ctrl = BackoffController::new(RestartPolicy::for_testing());
        ctrl.record_failure();
        ctrl.record_failure();

        // Not enough elapsed time
        ctrl.maybe_reset(Duration::from_secs(1));
        assert_eq!(ctrl.attempt_count(), 2);

        // Enough elapsed time (reset_after = 5s for testing)
        ctrl.maybe_reset(Duration::from_secs(6));
        assert_eq!(ctrl.attempt_count(), 0);
    }

    #[tokio::test]
    async fn test_backoff_controller_cancel() {
        let ctrl = BackoffController::new(RestartPolicy {
            initial_delay: Duration::from_secs(60), // very long
            ..RestartPolicy::for_testing()
        });
        let token = CancellationToken::new();
        let token_clone = token.clone();

        // Cancel immediately in a background task
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            token_clone.cancel();
        });

        let result = ctrl.wait_or_cancel(&token).await;
        assert!(!result, "Should return false when cancelled");
    }
}
