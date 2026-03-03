//! Shared retry, backoff, and scaling policies.
//!
//! This module provides three complementary types:
//!
//! - [`RestartPolicy`]: Stateless backoff configuration (initial delay, max
//!   delay, jitter, wave timeouts). Shared across service supervisors and
//!   trigger retry interceptors.
//! - [`ScalingPolicy`]: Stateless elastic-scaling configuration (concurrency
//!   limits, pressure threshold, cooldown). Only relevant for streaming
//!   trigger templates (e.g. `Queue`). Declared via
//!   [`TriggerHost::scaling_policy()`](crate::models::trigger::TriggerHost::scaling_policy) and optionally overridden by the user
//!   via [`ServiceDaemonBuilder::with_trigger_config`](crate::ServiceDaemonBuilder::with_trigger_config).
//! - [`BackoffController`]: A **stateful** controller that tracks the current
//!   backoff delay and attempt count. It wraps a `RestartPolicy` and provides
//!   interruption-aware waiting via `tokio::select!`.

use rand::RngExt;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use tracing::info;

// ---------------------------------------------------------------------------
// RestartPolicy -- stateless backoff configuration
// ---------------------------------------------------------------------------

/// Configuration for retry / restart behavior with exponential backoff.
///
/// This struct is **stateless** -- it only describes *how* to compute delays
/// for service restarts and trigger handler retries.
/// Pair it with [`BackoffController`] to get a stateful retry loop.
///
/// For elastic-scaling configuration (concurrency, pressure threshold),
/// see [`ScalingPolicy`].
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
    /// Maximum number of consecutive retries for **trigger handlers** before
    /// giving up on a single message.
    ///
    /// `None` means unlimited retries, which is the **designed default**.
    /// The framework intentionally retries trigger handlers forever, relying
    /// on exponential backoff and shutdown signals to terminate retry loops.
    ///
    /// Set this to `Some(n)` only when specific trigger handlers should not
    /// retry indefinitely (e.g., a payment processor where repeated failures
    /// indicate a permanent upstream issue).
    ///
    /// # Service retry behavior
    ///
    /// This setting does **not** affect services. Services are long-running
    /// background tasks that **always retry forever** by design. To
    /// permanently stop a service, return [`ServiceError::Fatal`](crate::ServiceError::Fatal) from the
    /// service function instead of relying on this configuration.
    pub trigger_max_retries: Option<u32>,
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
            trigger_max_retries: None, // Unlimited trigger retries by design
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
            trigger_max_retries: None, // Unlimited trigger retries by design
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

    /// Set the maximum number of consecutive retries for **trigger handlers**
    /// before giving up on a single message.
    ///
    /// By default, trigger retries are unlimited (`None`). Use this to add
    /// an explicit safety valve for handlers that should not retry forever.
    ///
    /// # Service retry behavior
    ///
    /// This does **not** affect services. Services always retry forever by
    /// design. To permanently stop a service, return [`ServiceError::Fatal`](crate::ServiceError::Fatal)
    /// from the service function instead.
    ///
    /// # Arguments
    /// * `count` - Maximum retry attempts per trigger message. After this
    ///   many consecutive failures, the retry interceptor will propagate
    ///   the error.
    pub fn trigger_max_retries(mut self, count: u32) -> Self {
        self.policy.trigger_max_retries = Some(count);
        self
    }

    #[must_use]
    pub fn build(self) -> RestartPolicy {
        self.policy
    }
}

// ---------------------------------------------------------------------------
// ScalingPolicy -- elastic scaling configuration for trigger concurrency
// ---------------------------------------------------------------------------

/// Configuration for elastic-scaling of trigger handler concurrency.
///
/// This struct is **stateless** and describes *how* the `TriggerRunner`
/// should manage concurrent handler dispatch for streaming event sources.
///
/// Trigger templates declare whether they need scaling via
/// [`TriggerHost::scaling_policy()`](crate::models::trigger::TriggerHost::scaling_policy). Only streaming templates (e.g.
/// `TopicHost` for `Queue`) return `Some(ScalingPolicy)`. Discrete
/// templates (`SignalHost`, `CronHost`, `WatchHost`) return `None`,
/// which disables the scale monitor and runs handlers serially.
///
/// Users can override the template default via
/// [`ServiceDaemonBuilder::with_trigger_config`](crate::ServiceDaemonBuilder::with_trigger_config).
#[derive(Debug, Clone, Copy)]
pub struct ScalingPolicy {
    /// Number of concurrent handler instances at cold-start (default: 1).
    ///
    /// The trigger runner starts with this many dispatch slots and scales
    /// up only when the pressure ratio exceeds `scale_threshold`.
    pub initial_concurrency: usize,
    /// Hard upper limit on concurrent handler instances (default: 64).
    ///
    /// The auto-scaler will never exceed this value, even under sustained
    /// high pressure. This acts as a safety guard against unbounded
    /// resource consumption.
    pub max_concurrency: usize,
    /// Multiplier applied to the current concurrency limit on each
    /// scale-up event (default: 2).
    ///
    /// For example, with `scale_factor = 2`, limits grow as:
    /// 1 → 2 → 4 → 8 → ... → `max_concurrency`.
    pub scale_factor: usize,
    /// Pressure ratio threshold that triggers a scale-up (default: 5).
    ///
    /// Pressure ratio is defined as `queue_depth / current_instances`.
    /// A threshold of 5 means: "if the backlog would take 5 processing
    /// cycles to drain at the current rate, scale up". Backlogs that
    /// can be consumed within fewer cycles are not worth scaling for.
    pub scale_threshold: usize,
    /// Duration of queue idleness before the runner starts reclaiming
    /// excess handler instances (default: 30 seconds).
    ///
    /// After the queue has been empty for this long, the runner
    /// shrinks concurrency back towards `initial_concurrency`.
    pub scale_cooldown: Duration,
}

impl Default for ScalingPolicy {
    fn default() -> Self {
        Self {
            initial_concurrency: 1,
            max_concurrency: 64,
            scale_factor: 2,
            scale_threshold: 5,
            scale_cooldown: Duration::from_secs(30),
        }
    }
}

impl ScalingPolicy {
    /// Create a scaling policy builder.
    pub fn builder() -> ScalingPolicyBuilder {
        ScalingPolicyBuilder::default()
    }

    /// Create a scaling policy for testing with smaller limits.
    pub fn for_testing() -> Self {
        Self {
            initial_concurrency: 1,
            max_concurrency: 4,
            scale_factor: 2,
            scale_threshold: 2,
            scale_cooldown: Duration::from_secs(2),
        }
    }
}

/// Builder for [`ScalingPolicy`].
#[derive(Default)]
pub struct ScalingPolicyBuilder {
    policy: ScalingPolicy,
}

impl ScalingPolicyBuilder {
    /// Set the initial number of concurrent handler instances.
    pub fn initial_concurrency(mut self, count: usize) -> Self {
        self.policy.initial_concurrency = count.max(1);
        self
    }

    /// Set the hard upper limit on concurrent handler instances.
    pub fn max_concurrency(mut self, count: usize) -> Self {
        self.policy.max_concurrency = count.max(1);
        self
    }

    /// Set the scale-up multiplier (e.g. 2 means double on each scale event).
    pub fn scale_factor(mut self, factor: usize) -> Self {
        self.policy.scale_factor = factor.max(2);
        self
    }

    /// Set the pressure ratio threshold that triggers scale-up.
    ///
    /// Pressure ratio = `queue_depth / current_instances`.
    /// A value of 5 means: "scale up when the backlog would take 5+
    /// processing cycles to drain".
    pub fn scale_threshold(mut self, threshold: usize) -> Self {
        self.policy.scale_threshold = threshold.max(1);
        self
    }

    /// Set the idle duration before the runner starts shrinking concurrency.
    pub fn scale_cooldown(mut self, duration: Duration) -> Self {
        self.policy.scale_cooldown = duration;
        self
    }

    #[must_use]
    pub fn build(self) -> ScalingPolicy {
        let mut policy = self.policy;
        // Enforce invariant: initial_concurrency must not exceed max_concurrency.
        // Auto-clamp rather than panic so the builder remains infallible.
        policy.initial_concurrency = policy.initial_concurrency.min(policy.max_concurrency);
        policy
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
    /// Internally delegates to [`record_failure`](Self::record_failure) to keep `attempt_count`
    /// and `current_delay` in sync.
    ///
    /// Returns `true` if retry should proceed, `false` if cancelled.
    pub async fn backoff_or_cancel(&mut self, cancel_token: &CancellationToken) -> bool {
        let proceed = self.wait_or_cancel(cancel_token).await;
        if proceed {
            self.record_failure();
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
    fn test_scaling_policy_default() {
        let policy = ScalingPolicy::default();
        assert_eq!(policy.initial_concurrency, 1);
        assert_eq!(policy.max_concurrency, 64);
        assert_eq!(policy.scale_factor, 2);
        assert_eq!(policy.scale_threshold, 5);
        assert_eq!(policy.scale_cooldown, Duration::from_secs(30));
    }

    #[test]
    fn test_scaling_policy_builder() {
        let policy = ScalingPolicy::builder()
            .initial_concurrency(4)
            .max_concurrency(2048)
            .scale_factor(3)
            .scale_threshold(10)
            .scale_cooldown(Duration::from_secs(60))
            .build();
        assert_eq!(policy.initial_concurrency, 4);
        assert_eq!(policy.max_concurrency, 2048);
        assert_eq!(policy.scale_factor, 3);
        assert_eq!(policy.scale_threshold, 10);
        assert_eq!(policy.scale_cooldown, Duration::from_secs(60));
    }

    #[test]
    fn test_scaling_policy_builder_clamping() {
        // initial_concurrency minimum is 1
        let policy = ScalingPolicy::builder().initial_concurrency(0).build();
        assert_eq!(policy.initial_concurrency, 1);
        // scale_factor minimum is 2
        let policy = ScalingPolicy::builder().scale_factor(1).build();
        assert_eq!(policy.scale_factor, 2);
        // scale_threshold minimum is 1
        let policy = ScalingPolicy::builder().scale_threshold(0).build();
        assert_eq!(policy.scale_threshold, 1);
    }

    #[test]
    fn test_scaling_policy_builder_initial_exceeds_max() {
        // When initial_concurrency > max_concurrency, build() should clamp it
        let policy = ScalingPolicy::builder()
            .initial_concurrency(64)
            .max_concurrency(4)
            .build();
        assert_eq!(
            policy.initial_concurrency, 4,
            "initial_concurrency must be clamped to max_concurrency"
        );
        assert_eq!(policy.max_concurrency, 4);
    }

    #[tokio::test]
    async fn test_backoff_or_cancel_increments_attempt() {
        let policy = RestartPolicy {
            initial_delay: Duration::from_millis(1),
            jitter_factor: 0.0,
            ..RestartPolicy::for_testing()
        };
        let mut ctrl = BackoffController::new(policy);
        assert_eq!(ctrl.attempt_count(), 0);

        let token = CancellationToken::new();
        let proceed = ctrl.backoff_or_cancel(&token).await;
        assert!(proceed, "Should proceed when not cancelled");
        assert_eq!(
            ctrl.attempt_count(),
            1,
            "attempt_count must increment after backoff_or_cancel"
        );
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

    // -----------------------------------------------------------------------
    // trigger_max_retries tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_restart_policy_default_trigger_max_retries_is_none() {
        let policy = RestartPolicy::default();
        assert_eq!(
            policy.trigger_max_retries, None,
            "Default trigger_max_retries must be None (unlimited retries by design)"
        );
    }

    #[test]
    fn test_restart_policy_for_testing_trigger_max_retries_is_none() {
        let policy = RestartPolicy::for_testing();
        assert_eq!(
            policy.trigger_max_retries, None,
            "for_testing() trigger_max_retries must be None (consistent with design)"
        );
    }

    #[test]
    fn test_restart_policy_builder_trigger_max_retries() {
        let policy = RestartPolicy::builder().trigger_max_retries(5).build();
        assert_eq!(policy.trigger_max_retries, Some(5));

        // Verify other fields remain at defaults
        assert_eq!(policy.initial_delay, Duration::from_secs(1));
        assert_eq!(policy.max_delay, Duration::from_secs(300));
    }

    #[test]
    fn test_restart_policy_builder_without_trigger_max_retries() {
        // Builder without calling trigger_max_retries() should produce None
        let policy = RestartPolicy::builder()
            .initial_delay(Duration::from_millis(500))
            .build();
        assert_eq!(
            policy.trigger_max_retries, None,
            "Builder without trigger_max_retries() must produce None"
        );
    }
}
