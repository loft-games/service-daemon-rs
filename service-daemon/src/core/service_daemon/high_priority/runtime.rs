use super::feedback::{Decision, Effect, EffectKind};
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::runtime::Handle;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::core::adaptive_scheduling::run_adaptive_scheduling_recommendations;
use crate::core::diagnostics::{
    DiagnosticsSnapshot, ObservationStatsSnapshot, RuntimeLane,
    run_high_priority_shard_runtime_probe, run_lane_runtime_probe,
};
use crate::core::service_daemon::high_priority::{
    HighPriorityPlacementDecision, HighPriorityPlacementDecisionKind, HighPriorityPlacementReason,
    observation_has_pressure,
};
use crate::models::{
    HighPriorityShardId, HighPriorityShardPressureState, ServiceDescription, ServiceScheduling,
};

use super::super::DaemonInstanceInner;

const HIGH_PRIORITY_POLICY_INTERVAL: Duration = Duration::from_millis(250);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::core::service_daemon) struct HighPriorityCapacityPlan {
    entry_count: usize,
    worker_count: Option<NonZeroUsize>,
}

impl HighPriorityCapacityPlan {
    pub(in crate::core::service_daemon) fn from_services(services: &[ServiceDescription]) -> Self {
        let entry_count = services
            .iter()
            .filter(|service| service.scheduling() == ServiceScheduling::HighPriority)
            .count();

        Self::from_entry_count(entry_count, read_available_parallelism())
    }

    pub(in crate::core::service_daemon) fn from_entry_count(
        entry_count: usize,
        available_parallelism: Option<NonZeroUsize>,
    ) -> Self {
        let worker_count = if entry_count == 0 {
            None
        } else {
            let cap = match available_parallelism {
                Some(parallelism) => parallelism.get(),
                None => 1,
            };
            NonZeroUsize::new(entry_count.min(cap))
        };

        Self {
            entry_count,
            worker_count,
        }
    }

    pub(in crate::core::service_daemon) fn entry_count(&self) -> usize {
        self.entry_count
    }

    pub(in crate::core::service_daemon) fn worker_count(&self) -> Option<NonZeroUsize> {
        self.worker_count
    }
}

fn read_available_parallelism() -> Option<NonZeroUsize> {
    std::thread::available_parallelism().ok()
}

impl DaemonInstanceInner {
    pub(in crate::core::service_daemon) fn ensure_high_priority_runtime(
        &mut self,
    ) -> std::io::Result<Option<Handle>> {
        let Some(worker_count) = self.high_priority_capacity.worker_count() else {
            return Ok(None);
        };

        if self.high_priority_runtime_pool.is_empty() {
            tracing::info!(
                high_priority_entries = self.high_priority_capacity.entry_count(),
                high_priority_worker_threads = worker_count.get(),
                "Creating initial high-priority runtime shard from static capacity plan"
            );
        }

        let handle = self.high_priority_runtime_pool.prepare_initial_runtime()?;
        self.resources
            .runtime_facts
            .record_high_priority_shards(self.high_priority_runtime_pool.snapshot());
        Ok(handle)
    }

    pub(in crate::core::service_daemon) fn spawn_runtime_probe(
        &mut self,
        handle: &Handle,
        lane: RuntimeLane,
    ) {
        let diagnostics = self.diagnostics.clone();
        let token = self.cancellation_token.clone();
        self.runtime_probe_tasks
            .push(handle.spawn(run_lane_runtime_probe(diagnostics, lane, token)));
    }

    pub(in crate::core::service_daemon) fn spawn_high_priority_runtime_probes(&mut self) {
        let Some(probe_runtime) = self
            .control_runtime
            .as_ref()
            .map(|runtime| runtime.handle().clone())
        else {
            return;
        };
        let diagnostics = self.diagnostics.clone();
        let token = self.cancellation_token.clone();
        for shard in self.high_priority_runtime_pool.shard_handles() {
            self.spawn_high_priority_runtime_probe(
                probe_runtime.clone(),
                shard.handle.clone(),
                shard.shard_id,
                diagnostics.clone(),
                token.clone(),
            );
        }
    }

    fn spawn_high_priority_runtime_probe(
        &mut self,
        probe_runtime: Handle,
        shard_runtime: Handle,
        shard_id: HighPriorityShardId,
        diagnostics: std::sync::Arc<crate::core::diagnostics::DiagnosticsStore>,
        token: CancellationToken,
    ) {
        self.runtime_probe_tasks
            .push(probe_runtime.spawn(run_high_priority_shard_runtime_probe(
                diagnostics,
                shard_id,
                shard_runtime,
                token,
            )));
    }

    pub(in crate::core::service_daemon) fn spawn_adaptive_recommendation_loop(
        &mut self,
        handle: &Handle,
    ) {
        if self.adaptive_recommendation_task.is_some()
            || !self.scheduling_advisory_profile.is_enabled()
        {
            return;
        }

        let diagnostics = self.diagnostics.clone();
        let token = self.cancellation_token.clone();
        self.adaptive_recommendation_task =
            Some(handle.spawn(run_adaptive_scheduling_recommendations(diagnostics, token)));
    }

    pub(in crate::core::service_daemon) fn spawn_high_priority_policy_loop(
        &mut self,
        handle: &Handle,
        inner: Arc<Mutex<DaemonInstanceInner>>,
    ) {
        if self.high_priority_policy_task.is_some() || self.high_priority_runtime_pool.is_empty() {
            return;
        }

        let token = self.cancellation_token.clone();
        self.high_priority_policy_task =
            Some(handle.spawn(run_high_priority_runtime_policy_loop(inner, token)));
    }

    pub(in crate::core::service_daemon) async fn stop_runtime_probes(&mut self) {
        for handle in self.runtime_probe_tasks.drain(..) {
            if let Err(err) = handle.await
                && !err.is_cancelled()
            {
                tracing::warn!(error = ?err, "Runtime probe task ended unexpectedly");
            }
        }
    }

    pub(in crate::core::service_daemon) async fn stop_adaptive_recommendation_loop(&mut self) {
        if let Some(handle) = self.adaptive_recommendation_task.take()
            && let Err(err) = handle.await
            && !err.is_cancelled()
        {
            tracing::error!(error = ?err, "Adaptive scheduling recommendation task ended unexpectedly");
        }
    }

    pub(in crate::core::service_daemon) async fn stop_high_priority_policy_loop(&mut self) {
        if let Some(handle) = self.high_priority_policy_task.take()
            && let Err(err) = handle.await
            && !err.is_cancelled()
        {
            tracing::error!(error = ?err, "HighPriority runtime policy loop ended unexpectedly");
        }
    }

    pub(in crate::core::service_daemon) fn abort_adaptive_recommendation_loop(&mut self) {
        if let Some(handle) = self.adaptive_recommendation_task.take() {
            handle.abort();
        }
    }

    pub(in crate::core::service_daemon) fn abort_high_priority_policy_loop(&mut self) {
        if let Some(handle) = self.high_priority_policy_task.take() {
            handle.abort();
        }
    }

    pub(in crate::core::service_daemon) fn shutdown_high_priority_runtime(&mut self) {
        self.high_priority_runtime_pool.shutdown();
        self.resources
            .runtime_facts
            .record_high_priority_shards(Vec::new());
    }

    pub(in crate::core::service_daemon) fn shutdown_high_priority_runtime_detached(&mut self) {
        self.high_priority_runtime_pool.shutdown_detached();
        self.resources
            .runtime_facts
            .record_high_priority_shards(Vec::new());
    }
}

async fn run_high_priority_runtime_policy_loop(
    inner: Arc<Mutex<DaemonInstanceInner>>,
    token: CancellationToken,
) {
    loop {
        tokio::select! {
            _ = tokio::time::sleep(HIGH_PRIORITY_POLICY_INTERVAL) => {
                tokio::select! {
                    mut inner = inner.lock() => {
                        inner.evaluate_high_priority_runtime_policy(Instant::now());
                    }
                    _ = token.cancelled() => break,
                }
            }
            _ = token.cancelled() => break,
        }
    }
}

impl DaemonInstanceInner {
    pub(in crate::core::service_daemon) fn evaluate_high_priority_runtime_policy(
        &mut self,
        now: Instant,
    ) {
        self.evaluate_high_priority_runtime_policy_with_settle(now, settle_time());
    }

    #[cfg(test)]
    pub(in crate::core::service_daemon) fn evaluate_high_priority_runtime_policy_for_test(
        &mut self,
        now: Instant,
    ) {
        self.evaluate_high_priority_runtime_policy_with_settle(now, Duration::ZERO);
    }

    fn evaluate_high_priority_runtime_policy_with_settle(
        &mut self,
        now: Instant,
        settling: Duration,
    ) {
        if self.high_priority_runtime_pool.is_empty() {
            return;
        }

        let policy = self.high_priority_runtime_pool.policy();
        let snapshot = self.diagnostics.snapshot();
        let mut pressured_shards = Vec::new();
        for shard in self.high_priority_runtime_pool.snapshot() {
            let observation = snapshot
                .high_priority_shards
                .iter()
                .find(|observed| observed.shard_id == shard.shard_id)
                .map(|observed| &observed.recent_runtime_probe);
            let pressure_state = match observation {
                Some(observation)
                    if observation.completed >= policy.minimum_completed_samples()
                        && observation_has_pressure(
                            observation.completed,
                            observation.avg_drift_ms,
                            policy,
                        ) =>
                {
                    pressured_shards.push(shard.shard_id);
                    HighPriorityShardPressureState::Pressured
                }
                Some(observation)
                    if observation.completed >= policy.minimum_completed_samples() =>
                {
                    HighPriorityShardPressureState::Nominal
                }
                _ => HighPriorityShardPressureState::Unknown,
            };
            self.high_priority_runtime_pool
                .state()
                .record_pressure(shard.shard_id, pressure_state);
            self.diagnostics
                .record_high_priority_shard_pressure_state(shard.shard_id, pressure_state);
        }

        self.high_priority_runtime_pool.feedback.retain(|instance| {
            snapshot
                .services
                .iter()
                .any(|service| service.service_instance_id == instance)
                && !self
                    .resources
                    .status_plane
                    .get(&instance)
                    .is_some_and(|status| {
                        matches!(
                            *status,
                            crate::models::ServiceStatus::Terminated
                                | crate::models::ServiceStatus::ShuttingDown
                        )
                    })
        });
        if let Some(effect) = self.high_priority_runtime_pool.feedback.expire(now) {
            self.high_priority_runtime_pool
                .state()
                .cancel_rollover(effect.before.instance);
            log_effect(effect);
        }
        let mut candidate = None;
        for shard in self.high_priority_runtime_pool.snapshot() {
            for (instance, generation) in self
                .high_priority_runtime_pool
                .state()
                .active_on_shard(shard.shard_id)
            {
                if self.is_lifecycle_operation_pending(instance) {
                    continue;
                }
                if self
                    .high_priority_runtime_pool
                    .state()
                    .take_external_reload(instance, generation)
                {
                    self.high_priority_runtime_pool
                        .feedback
                        .external_reload(instance);
                }
                let since = self
                    .high_priority_runtime_pool
                    .feedback
                    .since(instance, generation);
                let Some(sample) = self
                    .diagnostics
                    .high_priority_sleep_sample(instance, generation, now, since, settling)
                else {
                    continue;
                };
                match self.high_priority_runtime_pool.feedback.observe(
                    sample,
                    policy.minimum_completed_samples(),
                    policy.high_avg_drift_ms().saturating_mul(1_000_000),
                    policy.pressure_windows(),
                ) {
                    Decision::Candidate(sample) if pressured_shards.contains(&sample.shard) => {
                        if candidate.is_none() {
                            candidate = Some(sample);
                        }
                    }
                    Decision::Evaluated(effect) => log_effect(effect),
                    _ => {}
                }
            }
        }
        self.sync_high_priority_runtime_facts();
        let Some(candidate) = candidate else {
            self.record_high_priority_policy_suppressed(
                HighPriorityPlacementReason::InsufficientSamples,
            );
            return;
        };
        if global_runtime_pressure(&snapshot, policy) {
            self.record_high_priority_policy_suppressed(
                HighPriorityPlacementReason::GlobalPressure,
            );
            return;
        }
        if !self.high_priority_runtime_pool.rollover_allowed(now) {
            self.record_high_priority_policy_suppressed(HighPriorityPlacementReason::Cooldown);
            return;
        }
        let Some(signal) = self
            .resources
            .reload_signals
            .get(&candidate.instance)
            .map(|signal| signal.clone())
        else {
            return;
        };
        let scale_out = self.high_priority_runtime_pool.scale_out(now);
        let target = match scale_out {
            Ok(Some(shard_id)) => {
                self.record_high_priority_policy_decision(HighPriorityPlacementDecision {
                    shard_id: Some(shard_id),
                    kind: HighPriorityPlacementDecisionKind::ScaleOut,
                    reason: HighPriorityPlacementReason::PressureScaleOut,
                });
                if let Some(shard) = self.high_priority_runtime_pool.shard_handle(shard_id)
                    && let Some(probe_runtime) = self
                        .control_runtime
                        .as_ref()
                        .map(|runtime| runtime.handle().clone())
                {
                    self.spawn_high_priority_runtime_probe(
                        probe_runtime,
                        shard.handle,
                        shard.shard_id,
                        self.diagnostics.clone(),
                        self.cancellation_token.clone(),
                    );
                }
                Some(shard_id)
            }
            Ok(None) => {
                self.sync_scale_out_suppression_to_diagnostics(now);
                self.high_priority_runtime_pool
                    .snapshot()
                    .into_iter()
                    .filter(|shard| {
                        shard.shard_id != candidate.shard
                            && shard.pressure_state == HighPriorityShardPressureState::Nominal
                    })
                    .min_by_key(|shard| (shard.active_generations, shard.shard_id))
                    .map(|shard| shard.shard_id)
            }
            Err(error) => {
                tracing::warn!(%error, service_instance_id = %candidate.instance, "HighPriority shard allocation failed; intervention was not started");
                None
            }
        };
        if let Some(target) = target
            && self.high_priority_runtime_pool.state().request_rollover(
                candidate.instance,
                candidate.generation,
                target,
            )
        {
            let workers = self.high_priority_runtime_pool.total_worker_threads();
            self.high_priority_runtime_pool
                .feedback
                .start(candidate, target, workers, now);
            self.high_priority_runtime_pool.record_rollover_batch(now);
            self.record_high_priority_policy_decision(HighPriorityPlacementDecision {
                shard_id: Some(target),
                kind: HighPriorityPlacementDecisionKind::Rollover,
                reason: HighPriorityPlacementReason::PolicyRollover,
            });
            tracing::info!(service_instance_id = %candidate.instance, generation = candidate.generation, source_shard = %candidate.shard, target_shard = %target, metric = "service_sleep.mean_drift_ns", baseline = candidate.window.mean_drift_ns, samples = candidate.window.completed, workers, "HighPriority resource intervention requested");
            signal.notify_one();
        } else {
            self.record_high_priority_policy_suppressed(HighPriorityPlacementReason::NoBetterShard);
        }
        self.sync_high_priority_runtime_facts();
    }

    fn sync_scale_out_suppression_to_diagnostics(&self, _now: Instant) {
        let reason = if self.high_priority_runtime_pool.total_worker_threads()
            >= self.high_priority_runtime_pool.max_worker_threads()
        {
            HighPriorityPlacementReason::MaxCapacity
        } else {
            HighPriorityPlacementReason::Cooldown
        };
        self.record_high_priority_policy_suppressed(reason);
    }

    fn record_high_priority_policy_suppressed(&self, reason: HighPriorityPlacementReason) {
        self.record_high_priority_policy_decision(HighPriorityPlacementDecision {
            shard_id: None,
            kind: HighPriorityPlacementDecisionKind::Suppressed,
            reason,
        });
    }

    fn record_high_priority_policy_decision(&self, decision: HighPriorityPlacementDecision) {
        self.diagnostics
            .record_high_priority_placement_decision(decision);
        tracing::debug!(
            high_priority_shard_id = ?decision.shard_id,
            decision_kind = ?decision.kind,
            decision_reason = ?decision.reason,
            "HighPriority runtime placement decision"
        );
    }

    fn sync_high_priority_runtime_facts(&self) {
        self.resources
            .runtime_facts
            .record_high_priority_shards(self.high_priority_runtime_pool.snapshot());
    }
}

fn global_runtime_pressure(
    snapshot: &DiagnosticsSnapshot,
    policy: crate::models::policy::HighPriorityRuntimeControl,
) -> bool {
    lane_runtime_probe_pressure(snapshot, RuntimeLane::Control, policy)
        && lane_runtime_probe_pressure(snapshot, RuntimeLane::Standard, policy)
}

fn lane_runtime_probe_pressure(
    snapshot: &DiagnosticsSnapshot,
    lane: RuntimeLane,
    policy: crate::models::policy::HighPriorityRuntimeControl,
) -> bool {
    snapshot
        .lanes
        .iter()
        .find(|snapshot| snapshot.runtime_lane == lane)
        .is_some_and(|snapshot| observation_pressure(&snapshot.recent_runtime_probe, policy))
}

fn observation_pressure(
    observation: &ObservationStatsSnapshot,
    policy: crate::models::policy::HighPriorityRuntimeControl,
) -> bool {
    observation_has_pressure(observation.completed, observation.avg_drift_ms, policy)
}

fn settle_time() -> Duration {
    Duration::from_secs(2)
}

fn log_effect(effect: Effect) {
    let after_generation = effect.after.map(|sample| sample.generation);
    let actual_shard = effect.after.map(|sample| sample.shard);
    let after = effect.after.map(|sample| sample.window.mean_drift_ns);
    match effect.kind {
        EffectKind::PausedLowBenefit | EffectKind::PlacementUnchanged | EffectKind::TimedOut => {
            tracing::warn!(service_instance_id = %effect.before.instance, before_generation = effect.before.generation, ?after_generation, target_shard = %effect.target, ?actual_shard, worker_threads = effect.worker_threads, metric = "service_sleep.mean_drift_ns", before = effect.before.window.mean_drift_ns, ?after, reason = ?effect.kind, "HighPriority expansion paused; new comparable evidence is required");
        }
        _ => {
            tracing::info!(service_instance_id = %effect.before.instance, before_generation = effect.before.generation, ?after_generation, target_shard = %effect.target, ?actual_shard, worker_threads = effect.worker_threads, metric = "service_sleep.mean_drift_ns", before = effect.before.window.mean_drift_ns, ?after, reason = ?effect.kind, "HighPriority intervention evaluated")
        }
    }
}

#[cfg(test)]
mod timeout_tests {
    use super::*;
    use crate::models::ServiceInstanceId;
    #[test]
    fn high_priority_default_settling_is_not_shortened_in_test_builds() {
        assert_eq!(settle_time(), Duration::from_secs(2));
    }
    #[test]
    fn high_priority_timeout_pause_is_cleaned_on_remove_and_terminate() {
        for removed in [false, true] {
            let mut daemon = crate::ServiceDaemon::builder()
                .with_registry(
                    crate::Registry::builder()
                        .with_tag("__unit_high_priority_capacity_primary__")
                        .build(),
                )
                .build_inner();
            daemon.ensure_high_priority_runtime().unwrap();
            let instance = crate::models::ServiceInstanceId::new(uuid::Uuid::from_u128(9004));
            let base = Instant::now() + Duration::from_secs(3);
            daemon.diagnostics.register_generation(
                instance,
                "timeout",
                1,
                RuntimeLane::HighPriority,
            );
            let sample = super::super::feedback::ServiceSample {
                instance,
                generation: 1,
                shard: HighPriorityShardId(0),
                at: base,
                window: super::super::observation::SleepWindowSnapshot {
                    sequence: 8,
                    completed: 8,
                    mean_drift_ns: 200_000_000,
                    mean_requested_ns: 1_000_000,
                },
            };
            daemon
                .high_priority_runtime_pool
                .feedback
                .observe(sample, 1, 100_000_000, 1);
            daemon.high_priority_runtime_pool.feedback.start(
                sample,
                crate::models::HighPriorityShardId(1),
                2,
                base,
            );
            daemon
                .high_priority_runtime_pool
                .feedback
                .expire(base + Duration::from_secs(120))
                .unwrap();
            assert!(
                daemon
                    .high_priority_runtime_pool
                    .feedback
                    .since(instance, 1)
                    .is_some()
            );
            daemon
                .high_priority_runtime_pool
                .state()
                .remove_service_instance_generations(instance);
            if removed {
                daemon.diagnostics.remove_service_instance(instance);
            } else {
                daemon
                    .resources
                    .status_plane
                    .insert(instance, crate::models::ServiceStatus::Terminated);
            }
            daemon.evaluate_high_priority_runtime_policy(base + Duration::from_secs(121));
            assert!(
                daemon
                    .high_priority_runtime_pool
                    .feedback
                    .since(instance, 1)
                    .is_none()
            );
            let other = crate::core::service_daemon::high_priority::feedback::ServiceSample {
                instance: ServiceInstanceId::new(uuid::Uuid::from_u128(9005)),
                ..sample
            };
            assert!(matches!(
                daemon
                    .high_priority_runtime_pool
                    .feedback
                    .observe(other, 1, 100_000_000, 1),
                crate::core::service_daemon::high_priority::feedback::Decision::Candidate(_)
            ));
            daemon.shutdown_high_priority_runtime();
        }
    }
}
