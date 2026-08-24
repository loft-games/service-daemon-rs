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

use super::DaemonInstanceInner;

pub(super) const CONTROL_RUNTIME_WORKER_THREADS: usize = 1;
pub(super) const ISOLATED_STARTUP_CONCURRENCY_LIMIT: usize = 4;
const HIGH_PRIORITY_POLICY_INTERVAL: Duration = Duration::from_millis(250);

pub(super) struct PreparedRuntimes {
    pub(super) control: Option<Handle>,
    pub(super) standard: Handle,
    pub(super) high_priority: Option<Handle>,
}

pub(super) enum RuntimePreparationError {
    Control(std::io::Error),
    HighPriority(std::io::Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct HighPriorityCapacityPlan {
    entry_count: usize,
    worker_count: Option<NonZeroUsize>,
}

impl HighPriorityCapacityPlan {
    pub(super) fn from_services(services: &[ServiceDescription]) -> Self {
        let entry_count = services
            .iter()
            .filter(|service| service.scheduling() == ServiceScheduling::HighPriority)
            .count();

        Self::from_entry_count(entry_count, read_available_parallelism())
    }

    pub(super) fn from_entry_count(
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

    pub(super) fn entry_count(&self) -> usize {
        self.entry_count
    }

    pub(super) fn worker_count(&self) -> Option<NonZeroUsize> {
        self.worker_count
    }
}

fn read_available_parallelism() -> Option<NonZeroUsize> {
    std::thread::available_parallelism().ok()
}

impl DaemonInstanceInner {
    pub(super) fn prepare_startup_runtimes(
        &mut self,
    ) -> Result<PreparedRuntimes, RuntimePreparationError> {
        let control = if self.services.is_empty() {
            None
        } else {
            Some(
                self.ensure_control_runtime()
                    .map_err(RuntimePreparationError::Control)?,
            )
        };

        let high_priority = self
            .ensure_high_priority_runtime()
            .map_err(RuntimePreparationError::HighPriority)?;

        let standard = Handle::current();
        if let Some(runtime) = control.as_ref() {
            self.spawn_runtime_probe(runtime, RuntimeLane::Control);
            self.spawn_adaptive_recommendation_loop(runtime);
        }
        self.spawn_runtime_probe(&standard, RuntimeLane::Standard);
        if high_priority.is_some() {
            self.spawn_high_priority_runtime_probes();
        }

        Ok(PreparedRuntimes {
            control,
            standard,
            high_priority,
        })
    }

    pub(super) fn ensure_control_runtime(&mut self) -> std::io::Result<Handle> {
        if self.control_runtime.is_none() {
            self.control_runtime = Some(
                tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .worker_threads(CONTROL_RUNTIME_WORKER_THREADS)
                    .thread_name("svc-control")
                    .build()?,
            );
        }

        match self.control_runtime.as_ref() {
            Some(runtime) => Ok(runtime.handle().clone()),
            None => Err(std::io::Error::other(
                "control runtime missing after successful creation",
            )),
        }
    }

    pub(super) fn ensure_high_priority_runtime(&mut self) -> std::io::Result<Option<Handle>> {
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

    pub(super) fn spawn_runtime_probe(&mut self, handle: &Handle, lane: RuntimeLane) {
        let diagnostics = self.diagnostics.clone();
        let token = self.cancellation_token.clone();
        self.runtime_probe_tasks
            .push(handle.spawn(run_lane_runtime_probe(diagnostics, lane, token)));
    }

    pub(super) fn spawn_high_priority_runtime_probes(&mut self) {
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

    pub(super) fn spawn_adaptive_recommendation_loop(&mut self, handle: &Handle) {
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

    pub(super) fn spawn_high_priority_policy_loop(
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

    pub(super) async fn stop_runtime_probes(&mut self) {
        for handle in self.runtime_probe_tasks.drain(..) {
            if let Err(err) = handle.await
                && !err.is_cancelled()
            {
                tracing::warn!(error = ?err, "Runtime probe task ended unexpectedly");
            }
        }
    }

    pub(super) async fn stop_adaptive_recommendation_loop(&mut self) {
        if let Some(handle) = self.adaptive_recommendation_task.take()
            && let Err(err) = handle.await
            && !err.is_cancelled()
        {
            tracing::error!(error = ?err, "Adaptive scheduling recommendation task ended unexpectedly");
        }
    }

    pub(super) async fn stop_high_priority_policy_loop(&mut self) {
        if let Some(handle) = self.high_priority_policy_task.take()
            && let Err(err) = handle.await
            && !err.is_cancelled()
        {
            tracing::error!(error = ?err, "HighPriority runtime policy loop ended unexpectedly");
        }
    }

    pub(super) fn abort_adaptive_recommendation_loop(&mut self) {
        if let Some(handle) = self.adaptive_recommendation_task.take() {
            handle.abort();
        }
    }

    pub(super) fn abort_high_priority_policy_loop(&mut self) {
        if let Some(handle) = self.high_priority_policy_task.take() {
            handle.abort();
        }
    }

    pub(super) fn shutdown_high_priority_runtime(&mut self) {
        self.high_priority_runtime_pool.shutdown();
        self.resources
            .runtime_facts
            .record_high_priority_shards(Vec::new());
    }

    pub(super) fn shutdown_control_runtime(&mut self) {
        if let Some(runtime) = self.control_runtime.take()
            && let Err(panic) = std::thread::spawn(move || drop(runtime)).join()
        {
            tracing::error!(?panic, "Control runtime shutdown thread panicked");
        }
    }

    pub(super) fn shutdown_high_priority_runtime_detached(&mut self) {
        self.high_priority_runtime_pool.shutdown_detached();
        self.resources
            .runtime_facts
            .record_high_priority_shards(Vec::new());
    }

    pub(super) fn shutdown_control_runtime_detached(&mut self) {
        if let Some(runtime) = self.control_runtime.take() {
            let _ = std::thread::spawn(move || drop(runtime));
        }
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
    pub(super) fn evaluate_high_priority_runtime_policy(&mut self, now: Instant) {
        if self.high_priority_runtime_pool.is_empty() {
            return;
        }

        let policy = self.high_priority_runtime_pool.policy();
        let snapshot = self.diagnostics.snapshot();
        let mut pressured_shards = Vec::new();
        let mut enough_samples = false;
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
                    enough_samples = true;
                    pressured_shards.push(shard.shard_id);
                    HighPriorityShardPressureState::Pressured
                }
                Some(observation)
                    if observation.completed >= policy.minimum_completed_samples() =>
                {
                    enough_samples = true;
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

        if !enough_samples {
            self.record_high_priority_policy_suppressed(
                HighPriorityPlacementReason::InsufficientSamples,
            );
            self.sync_high_priority_runtime_facts();
            return;
        }

        let any_pressure = !pressured_shards.is_empty();
        if !self
            .high_priority_runtime_pool
            .record_pressure_window(any_pressure)
        {
            self.sync_high_priority_runtime_facts();
            return;
        }
        if !any_pressure {
            self.sync_high_priority_runtime_facts();
            return;
        }

        if global_runtime_pressure(&snapshot, policy) {
            self.record_high_priority_policy_suppressed(
                HighPriorityPlacementReason::GlobalPressure,
            );
            self.sync_high_priority_runtime_facts();
            return;
        }

        let scale_out = self.high_priority_runtime_pool.scale_out(now);
        let new_shard_id = match scale_out {
            Ok(shard_id) => shard_id,
            Err(err) => {
                tracing::error!(
                    error = %err,
                    "HighPriority runtime policy failed to create runtime shard"
                );
                None
            }
        };

        if let Some(shard_id) = new_shard_id {
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
            tracing::info!(
                high_priority_shard_id = %shard_id,
                "HighPriority runtime policy scaled out"
            );
        } else {
            self.sync_scale_out_suppression_to_diagnostics(now);
        }

        self.request_high_priority_rollovers(new_shard_id, &pressured_shards, now);
        self.sync_high_priority_runtime_facts();
    }

    fn request_high_priority_rollovers(
        &mut self,
        target_shard_id: Option<HighPriorityShardId>,
        pressured_shards: &[HighPriorityShardId],
        now: Instant,
    ) {
        let Some(target_shard_id) = target_shard_id.or_else(|| {
            self.high_priority_runtime_pool
                .snapshot()
                .into_iter()
                .filter(|shard| !pressured_shards.contains(&shard.shard_id))
                .min_by_key(|shard| (shard.active_generations, shard.shard_id))
                .map(|shard| shard.shard_id)
        }) else {
            self.record_high_priority_policy_suppressed(HighPriorityPlacementReason::NoBetterShard);
            return;
        };

        let policy = self.high_priority_runtime_pool.policy();
        if policy.max_rollovers_per_window() == 0 {
            self.record_high_priority_policy_suppressed(HighPriorityPlacementReason::NoBetterShard);
            return;
        }
        if !self.high_priority_runtime_pool.rollover_allowed(now) {
            self.record_high_priority_policy_suppressed(HighPriorityPlacementReason::Cooldown);
            return;
        }

        let mut remaining = self
            .high_priority_runtime_pool
            .policy()
            .max_rollovers_per_window();
        let mut requested = 0usize;
        for shard_id in pressured_shards {
            for (service_instance_id, generation) in self
                .high_priority_runtime_pool
                .state()
                .active_on_shard(*shard_id)
            {
                if remaining == 0 {
                    break;
                }
                if self.is_lifecycle_operation_pending(service_instance_id) {
                    continue;
                }
                if let Some(signal) = self.resources.reload_signals.get(&service_instance_id) {
                    if !self.high_priority_runtime_pool.state().request_rollover(
                        service_instance_id,
                        generation,
                        target_shard_id,
                    ) {
                        continue;
                    }
                    signal.notify_one();
                    let decision = HighPriorityPlacementDecision {
                        shard_id: Some(target_shard_id),
                        kind: HighPriorityPlacementDecisionKind::Rollover,
                        reason: HighPriorityPlacementReason::PolicyRollover,
                    };
                    self.record_high_priority_policy_decision(decision);
                    tracing::info!(
                        service_instance_id = %service_instance_id,
                        generation,
                        source_high_priority_shard_id = %shard_id,
                        target_high_priority_shard_id = %target_shard_id,
                        "HighPriority runtime policy requested cooperative generation rollover"
                    );
                    remaining -= 1;
                    requested += 1;
                }
            }
        }
        if requested > 0 {
            self.high_priority_runtime_pool.record_rollover_batch(now);
        } else {
            self.record_high_priority_policy_suppressed(HighPriorityPlacementReason::NoBetterShard);
        }
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
