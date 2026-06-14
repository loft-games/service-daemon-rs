use futures::future::{BoxFuture, pending};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::Semaphore;
use tokio::task::JoinHandle;
use tracing::info;

use crate::models::policy::ScalingPolicy;

use super::TriggerRunner;
use super::dispatch::{AbortOnDropJoinHandle, DispatchTaskOutcome};
use super::failure::{TriggerDispatchFailure, TriggerDispatchFailureKind};

impl<P: Send + Sync + 'static> TriggerRunner<P> {
    pub(super) fn scale_monitor_future(&self) -> BoxFuture<'static, DispatchTaskOutcome> {
        match self.scaling {
            Some(scaling) => self.observe_scale_monitor(self.spawn_scale_monitor(scaling)),
            None => Box::pin(pending::<DispatchTaskOutcome>()),
        }
    }

    // -----------------------------------------------------------------------
    // Elastic scaling -- background pressure monitor
    // -----------------------------------------------------------------------

    /// Spawn a background task that monitors semaphore pressure and
    /// dynamically adjusts concurrency limits.
    ///
    /// # Scaling Algorithm
    ///
    /// The monitor runs on a fixed interval (1 second) and computes:
    ///
    /// ```text
    /// in_flight = current_limit - available_permits
    /// pressure_ratio = in_flight / current_limit
    /// ```
    ///
    /// - **Scale Up**: When `pressure_ratio >= scale_threshold / (scale_threshold + 1)`,
    ///   meaning almost all permits are occupied, the limit is multiplied by
    ///   `scale_factor` (clamped to `max_concurrency`).
    /// - **Scale Down**: When no permits are in use for longer than
    ///   `scale_cooldown`, the limit shrinks back to `initial_concurrency`.
    ///
    /// New permits are added via `Semaphore::add_permits()`; shrinking is
    /// deferred -- we simply stop adding new permits and let the natural
    /// permit release bring the effective concurrency down.
    pub(super) fn spawn_scale_monitor(
        &self,
        scaling: ScalingPolicy,
    ) -> tokio::task::JoinHandle<()> {
        let semaphore = self.semaphore.clone();
        let current_limit = self.current_limit.clone();
        let trigger_name = self.name;

        tokio::spawn(async move {
            // Track how long the queue has been idle (all permits available)
            let mut idle_since: Option<Instant> = None;

            loop {
                tokio::time::sleep(Duration::from_secs(1)).await;

                let limit = current_limit.load(Ordering::Relaxed);
                let available = semaphore.available_permits();
                let in_flight = limit.saturating_sub(available);

                if in_flight == 0 {
                    idle_since.get_or_insert_with(Instant::now);
                    Self::try_scale_down(
                        &semaphore,
                        &current_limit,
                        &scaling,
                        trigger_name,
                        limit,
                        &mut idle_since,
                    );
                    continue;
                }

                // --- Path B: Active handlers present ---
                idle_since = None;
                Self::try_scale_up(
                    &semaphore,
                    &current_limit,
                    &scaling,
                    trigger_name,
                    limit,
                    in_flight,
                );
            }
        })
    }

    pub(super) fn observe_scale_monitor(
        &self,
        handle: JoinHandle<()>,
    ) -> BoxFuture<'static, DispatchTaskOutcome> {
        let trigger_name = self.name;
        let service_id = self.service_id;
        Box::pin(async move {
            let mut handle = AbortOnDropJoinHandle::new(handle);
            match handle.join().await {
                Ok(()) => Err(TriggerDispatchFailure::new(
                    TriggerDispatchFailureKind::ScaleMonitorFailed,
                    trigger_name,
                    service_id,
                    None,
                    None,
                    "scale monitor exited unexpectedly",
                )),
                Err(join_error) => Err(TriggerDispatchFailure::new(
                    TriggerDispatchFailureKind::ScaleMonitorFailed,
                    trigger_name,
                    service_id,
                    None,
                    None,
                    format!("scale monitor task failed: {join_error}"),
                )),
            }
        })
    }

    /// Attempt to scale down concurrency if the idle cooldown has elapsed.
    ///
    /// Called when `in_flight == 0`. Physically revokes excess permits by
    /// acquiring them via `try_acquire()` and calling `forget()` to permanently
    /// remove them from the semaphore. This ensures `dispatch` truly cannot
    /// exceed the reduced concurrency limit.
    ///
    /// Resets `idle_since` to `None` after successfully scaling down.
    pub(super) fn try_scale_down(
        semaphore: &Semaphore,
        current_limit: &AtomicUsize,
        scaling: &ScalingPolicy,
        trigger_name: &str,
        limit: usize,
        idle_since: &mut Option<Instant>,
    ) {
        let Some(since) = *idle_since else { return };
        let initial = scaling.initial_concurrency();
        if since.elapsed() < scaling.scale_cooldown() || limit <= initial {
            return;
        }

        // Physically revoke excess permits by acquiring and forgetting them.
        // `forget()` permanently reduces the semaphore capacity, ensuring
        // `dispatch` cannot acquire more permits than `initial_concurrency`.
        let to_revoke = limit.saturating_sub(initial);
        let mut revoked = 0usize;
        for _ in 0..to_revoke {
            match semaphore.try_acquire() {
                Ok(permit) => {
                    permit.forget();
                    revoked += 1;
                }
                Err(_) => break, // Rare race with new dispatch; stop early
            }
        }

        let new_limit = limit.saturating_sub(revoked);
        current_limit.store(new_limit, Ordering::Relaxed);
        info!(
            trigger = %trigger_name,
            old_limit = limit,
            new_limit,
            revoked,
            "Elastic scale-down: revoked {} permits",
            revoked
        );
        *idle_since = None;
    }

    pub(super) fn pressure_limit_for(limit: usize, threshold: usize) -> usize {
        let numerator = (limit as u128).saturating_mul(threshold as u128);
        let denominator = (threshold as u128).saturating_add(1);
        let pressure_limit = numerator / denominator;
        pressure_limit.min(usize::MAX as u128) as usize
    }

    pub(super) fn next_scaled_limit(limit: usize, scaling: &ScalingPolicy) -> usize {
        limit
            .saturating_mul(scaling.scale_factor())
            .min(scaling.max_concurrency())
    }

    /// Attempt to scale up concurrency if the pressure ratio exceeds the threshold.
    ///
    /// Pressure check: `in_flight >= limit * threshold / (threshold + 1)`.
    /// At default `threshold=5`, this fires at ~83% utilization.
    pub(super) fn try_scale_up(
        semaphore: &Semaphore,
        current_limit: &AtomicUsize,
        scaling: &ScalingPolicy,
        trigger_name: &str,
        limit: usize,
        in_flight: usize,
    ) {
        if limit >= scaling.max_concurrency() {
            return;
        }

        let pressure_limit = Self::pressure_limit_for(limit, scaling.scale_threshold());

        if in_flight < pressure_limit {
            return;
        }

        let new_limit = Self::next_scaled_limit(limit, scaling);

        if new_limit <= limit {
            return;
        }

        let added = new_limit - limit;
        semaphore.add_permits(added);
        current_limit.store(new_limit, Ordering::Relaxed);
        info!(
            trigger = %trigger_name,
            old_limit = limit,
            new_limit,
            in_flight,
            "Elastic scale-up: added {} permits",
            added
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use crate::models::policy::RestartPolicy;
    use crate::models::service::ServiceId;
    use crate::models::trigger::TriggerHandler;

    /// Verifies that the semaphore limits concurrent handler invocations
    /// to the configured `initial_concurrency`.
    #[tokio::test]
    async fn test_semaphore_limits_concurrency() {
        let scaling = ScalingPolicy::builder()
            .initial_concurrency(2)
            .max_concurrency(8)
            .build();

        let semaphore = Arc::new(Semaphore::new(scaling.initial_concurrency()));
        let current_limit = Arc::new(AtomicUsize::new(scaling.initial_concurrency()));

        // Acquire 2 permits -- should succeed (matches initial_concurrency)
        let _p1 = semaphore.clone().acquire_owned().await.unwrap();
        let _p2 = semaphore.clone().acquire_owned().await.unwrap();

        // Third acquire should not be immediately available
        let try_result = semaphore.clone().try_acquire_owned();
        assert!(
            try_result.is_err(),
            "Semaphore should be exhausted at initial_concurrency=2"
        );

        assert_eq!(current_limit.load(Ordering::Relaxed), 2);
    }

    /// Verifies that `add_permits` correctly expands the concurrency limit.
    #[tokio::test]
    async fn test_semaphore_scale_up() {
        let scaling = ScalingPolicy::builder()
            .initial_concurrency(1)
            .max_concurrency(4)
            .scale_factor(2)
            .build();

        let semaphore = Arc::new(Semaphore::new(scaling.initial_concurrency()));
        let current_limit = Arc::new(AtomicUsize::new(scaling.initial_concurrency()));

        TriggerRunner::<()>::try_scale_up(
            &semaphore,
            &current_limit,
            &scaling,
            "test_trigger",
            1,
            1,
        );

        assert_eq!(current_limit.load(Ordering::Relaxed), 2);
        assert_eq!(semaphore.available_permits(), 2); // 1 original + 1 added

        TriggerRunner::<()>::try_scale_up(
            &semaphore,
            &current_limit,
            &scaling,
            "test_trigger",
            2,
            2,
        );

        assert_eq!(current_limit.load(Ordering::Relaxed), 4);
        assert_eq!(semaphore.available_permits(), 4); // 2 previous + 2 added
    }

    /// Verifies that scale-up respects `max_concurrency` ceiling.
    #[tokio::test]
    async fn test_scale_up_respects_max_concurrency() {
        let scaling = ScalingPolicy::builder()
            .initial_concurrency(1)
            .max_concurrency(3)
            .scale_factor(4)
            .build();

        let semaphore = Arc::new(Semaphore::new(scaling.initial_concurrency()));
        let current_limit = Arc::new(AtomicUsize::new(scaling.initial_concurrency()));

        TriggerRunner::<()>::try_scale_up(
            &semaphore,
            &current_limit,
            &scaling,
            "test_trigger",
            1,
            1,
        );

        assert_eq!(current_limit.load(Ordering::Relaxed), 3);
        assert_eq!(semaphore.available_permits(), 3);
    }

    /// Verifies that scale-down physically revokes permits from the semaphore
    /// via `try_acquire()` + `forget()`, not just a logical counter update.
    #[tokio::test]
    async fn test_scale_down_physically_revokes_permits() {
        let scaling = ScalingPolicy::builder()
            .initial_concurrency(1)
            .max_concurrency(8)
            .scale_cooldown(Duration::from_millis(10))
            .build();

        // Simulate a scaled-up state: semaphore has 4 permits, limit = 4
        let semaphore = Arc::new(Semaphore::new(4));
        let current_limit = Arc::new(AtomicUsize::new(4));

        // Wait for cooldown to elapse
        tokio::time::sleep(Duration::from_millis(20)).await;

        let mut idle_since = Some(std::time::Instant::now() - Duration::from_millis(50));

        TriggerRunner::<()>::try_scale_down(
            &semaphore,
            &current_limit,
            &scaling,
            "test_trigger",
            4, // current limit
            &mut idle_since,
        );

        // Logical limit should be back to initial
        assert_eq!(current_limit.load(Ordering::Relaxed), 1);
        // Physical permits should also be reduced (4 - 3 revoked = 1)
        assert_eq!(
            semaphore.available_permits(),
            1,
            "Semaphore should have physically lost permits after scale-down"
        );
    }

    /// Verifies that a full scale-down -> scale-up roundtrip works correctly:
    /// permits are physically revoked during scale-down and physically restored
    /// during scale-up.
    #[tokio::test]
    async fn test_scale_down_then_scale_up_roundtrip() {
        let scaling = ScalingPolicy::builder()
            .initial_concurrency(2)
            .max_concurrency(8)
            .scale_factor(2)
            .scale_cooldown(Duration::from_millis(10))
            .build();

        // Start with a scaled-up state: 4 permits
        let semaphore = Arc::new(Semaphore::new(4));
        let current_limit = Arc::new(AtomicUsize::new(4));

        // --- Phase 1: Scale down from 4 -> 2 ---
        tokio::time::sleep(Duration::from_millis(20)).await;
        let mut idle_since = Some(std::time::Instant::now() - Duration::from_millis(50));

        TriggerRunner::<()>::try_scale_down(
            &semaphore,
            &current_limit,
            &scaling,
            "test_trigger",
            4,
            &mut idle_since,
        );

        assert_eq!(current_limit.load(Ordering::Relaxed), 2);
        assert_eq!(semaphore.available_permits(), 2);

        // --- Phase 2: Scale up from 2 -> 4 ---
        // Simulate pressure: acquire both permits so in_flight = 2
        let _p1 = semaphore.clone().try_acquire_owned().unwrap();
        let _p2 = semaphore.clone().try_acquire_owned().unwrap();

        TriggerRunner::<()>::try_scale_up(
            &semaphore,
            &current_limit,
            &scaling,
            "test_trigger",
            2, // current limit
            2, // in_flight (100% pressure)
        );

        assert_eq!(current_limit.load(Ordering::Relaxed), 4);
        // 2 new permits added, but 2 are held by _p1/_p2
        assert_eq!(semaphore.available_permits(), 2);
    }

    /// Verifies that the pressure calculation correctly identifies when
    /// scaling is needed.
    #[test]
    fn test_pressure_calculation() {
        // With threshold=5, pressure_limit = limit * 5 / 6
        let threshold: usize = 5;

        // Case 1: limit=1 -> pressure_limit = 0 -> any in_flight triggers scale
        let pressure_limit = TriggerRunner::<()>::pressure_limit_for(1, threshold);
        assert_eq!(pressure_limit, 0);
        assert!(
            1 >= pressure_limit,
            "Single in-flight should trigger scale-up"
        );

        // Case 2: limit=6 -> pressure_limit = 5 -> need 5+ in_flight to trigger
        let pressure_limit = TriggerRunner::<()>::pressure_limit_for(6, threshold);
        assert_eq!(pressure_limit, 5);
        assert!(5 >= pressure_limit, "5 of 6 should trigger scale-up");
        assert!(4 < pressure_limit, "4 of 6 should NOT trigger scale-up");

        // Case 3: limit=12 -> pressure_limit = 10
        let pressure_limit = TriggerRunner::<()>::pressure_limit_for(12, threshold);
        assert_eq!(pressure_limit, 10);
    }

    #[test]
    fn pressure_calculation_handles_large_values_without_overflow() {
        let pressure_limit = TriggerRunner::<()>::pressure_limit_for(usize::MAX, usize::MAX);

        assert_eq!(pressure_limit, usize::MAX - 1);
    }

    #[test]
    fn scale_up_calculation_saturates_at_max_concurrency() {
        let scaling = ScalingPolicy::try_new(1, usize::MAX, usize::MAX, 1, Duration::ZERO)
            .expect("valid extreme scaling policy should be accepted");

        assert_eq!(
            TriggerRunner::<()>::next_scaled_limit(usize::MAX - 1, &scaling),
            usize::MAX
        );
    }

    #[test]
    fn scale_up_ignores_non_growing_limit_without_underflow() {
        let scaling = ScalingPolicy::default();
        let semaphore = Semaphore::new(0);
        let current_limit = AtomicUsize::new(0);

        TriggerRunner::<()>::try_scale_up(
            &semaphore,
            &current_limit,
            &scaling,
            "test_trigger",
            0,
            0,
        );

        assert_eq!(current_limit.load(Ordering::Relaxed), 0);
        assert_eq!(semaphore.available_permits(), 0);
    }

    // -----------------------------------------------------------------------
    // New tests: TriggerRunner conditional scaling initialization
    // -----------------------------------------------------------------------

    /// When `scaling = None`, TriggerRunner should initialize with exactly
    /// 1 permit (serial dispatch) and current_limit = 1.
    #[test]
    fn test_runner_no_scaling_serial_dispatch() {
        let handler: TriggerHandler<String> = Arc::new(|_ctx| Box::pin(async { Ok(()) }));
        let runner = TriggerRunner::new(
            "test_no_scaling",
            ServiceId::new(99),
            handler,
            RestartPolicy::default(),
            None, // no scaling
        );

        // Serial mode: exactly 1 permit available
        assert_eq!(runner.semaphore.available_permits(), 1);
        assert_eq!(runner.current_limit.load(Ordering::Relaxed), 1);
        assert!(runner.scaling.is_none());
    }

    /// When `scaling = Some(ScalingPolicy)`, TriggerRunner should initialize
    /// with `initial_concurrency` permits and store the policy.
    #[test]
    fn test_runner_with_scaling_initializes_permits() {
        let sp = ScalingPolicy::builder()
            .initial_concurrency(4)
            .max_concurrency(16)
            .build();
        let handler: TriggerHandler<String> = Arc::new(|_ctx| Box::pin(async { Ok(()) }));
        let runner = TriggerRunner::new(
            "test_with_scaling",
            ServiceId::new(100),
            handler,
            RestartPolicy::default(),
            Some(sp),
        );

        assert_eq!(runner.semaphore.available_permits(), 4);
        assert_eq!(runner.current_limit.load(Ordering::Relaxed), 4);
        assert!(runner.scaling.is_some());
        let stored = runner.scaling.unwrap();
        assert_eq!(stored.max_concurrency(), 16);
    }

    /// Verify that the default ScalingPolicy (used by TopicHost) produces
    /// expected initial values in TriggerRunner.
    #[test]
    fn test_runner_default_scaling_policy_values() {
        let sp = ScalingPolicy::default();
        let handler: TriggerHandler<String> = Arc::new(|_ctx| Box::pin(async { Ok(()) }));
        let runner = TriggerRunner::new(
            "test_default_sp",
            ServiceId::new(101),
            handler,
            RestartPolicy::default(),
            Some(sp),
        );

        // Default initial_concurrency is 1
        assert_eq!(runner.semaphore.available_permits(), 1);
        assert_eq!(runner.current_limit.load(Ordering::Relaxed), 1);
        // But scaling IS enabled
        assert!(runner.scaling.is_some());
        assert_eq!(runner.scaling.unwrap().max_concurrency(), 64);
    }

    /// Verify that custom ScalingPolicy via builder integrates correctly
    /// with TriggerRunner initialization.
    #[test]
    fn test_runner_custom_scaling_via_builder() {
        let sp = ScalingPolicy::builder()
            .initial_concurrency(8)
            .max_concurrency(64)
            .scale_factor(4)
            .scale_threshold(3)
            .scale_cooldown(Duration::from_secs(10))
            .build();

        let handler: TriggerHandler<String> = Arc::new(|_ctx| Box::pin(async { Ok(()) }));
        let runner = TriggerRunner::new(
            "test_builder_sp",
            ServiceId::new(102),
            handler,
            RestartPolicy::default(),
            Some(sp),
        );

        assert_eq!(runner.semaphore.available_permits(), 8);
        assert_eq!(runner.current_limit.load(Ordering::Relaxed), 8);
        let stored = runner.scaling.unwrap();
        assert_eq!(stored.scale_factor(), 4);
        assert_eq!(stored.scale_threshold(), 3);
        assert_eq!(stored.scale_cooldown(), Duration::from_secs(10));
    }

    #[tokio::test]
    async fn scale_monitor_unexpected_completion_is_observable() {
        let handler: TriggerHandler<()> = Arc::new(|_ctx| Box::pin(async { Ok(()) }));
        let runner = TriggerRunner::new(
            "scale_monitor_trigger",
            ServiceId::new(205),
            handler,
            RestartPolicy::for_testing(),
            Some(ScalingPolicy::default()),
        );
        let monitor = tokio::spawn(async {});

        let result = runner.observe_scale_monitor(monitor).await;
        let failure = result.expect_err("scale monitor completion should be observable");
        assert_eq!(
            failure.kind(),
            TriggerDispatchFailureKind::ScaleMonitorFailed
        );
    }
}
