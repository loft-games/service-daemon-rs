use std::num::NonZeroUsize;
use std::sync::{Arc, RwLock};
use std::time::Instant;

use chrono::{DateTime, Utc};
use dashmap::DashMap;
use tokio::runtime::{Handle, Runtime};

use crate::models::policy::HighPriorityRuntimeControl;
use crate::models::{
    HighPriorityRuntimeShardSnapshot, HighPriorityShardId, HighPriorityShardPressureState,
    ServiceInstanceId,
};

use super::runtime::HighPriorityCapacityPlan;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HighPriorityPlacementDecisionKind {
    LeastLoaded,
    ScaleOut,
    Rollover,
    Suppressed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HighPriorityPlacementReason {
    LeastLoadedShard,
    PressureScaleOut,
    PolicyRollover,
    InsufficientSamples,
    Cooldown,
    MaxCapacity,
    GlobalPressure,
    NoBetterShard,
    NoHighPriorityRuntime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HighPriorityPlacementDecision {
    pub shard_id: Option<HighPriorityShardId>,
    pub kind: HighPriorityPlacementDecisionKind,
    pub reason: HighPriorityPlacementReason,
}

#[derive(Debug, Clone)]
pub(super) struct HighPriorityShardHandle {
    pub shard_id: HighPriorityShardId,
    pub worker_threads: usize,
    pub created_at: DateTime<Utc>,
    pub handle: Handle,
}

#[derive(Default)]
struct HighPriorityRuntimePoolStateInner {
    pressure_state: DashMap<HighPriorityShardId, HighPriorityShardPressureState>,
}

#[derive(Default)]
pub(super) struct HighPriorityRuntimePoolState {
    shards: RwLock<Vec<HighPriorityShardHandle>>,
    assignments: DashMap<ServiceInstanceId, HighPriorityShardId>,
    active_generations: DashMap<(ServiceInstanceId, u64), HighPriorityShardId>,
    pending_rollovers: DashMap<(ServiceInstanceId, u64), HighPriorityShardId>,
    inner: HighPriorityRuntimePoolStateInner,
}

impl HighPriorityRuntimePoolState {
    #[cfg(test)]
    pub(super) fn for_test_current_runtime() -> Arc<Self> {
        let state = Arc::new(Self::default());
        state
            .shards
            .write()
            .unwrap_or_else(|err| err.into_inner())
            .push(HighPriorityShardHandle {
                shard_id: HighPriorityShardId(0),
                worker_threads: 1,
                created_at: Utc::now(),
                handle: Handle::current(),
            });
        state.record_pressure(
            HighPriorityShardId(0),
            HighPriorityShardPressureState::Nominal,
        );
        state
    }

    pub(super) fn snapshot(&self) -> Vec<HighPriorityRuntimeShardSnapshot> {
        let shards = self.shards.read().unwrap_or_else(|err| err.into_inner());
        let mut snapshots: Vec<_> = shards
            .iter()
            .map(|shard| {
                let active_generations = self
                    .active_generations
                    .iter()
                    .filter(|entry| *entry.value() == shard.shard_id)
                    .count();
                let assigned_instances = self
                    .assignments
                    .iter()
                    .filter(|entry| *entry.value() == shard.shard_id)
                    .count();
                HighPriorityRuntimeShardSnapshot {
                    shard_id: shard.shard_id,
                    worker_threads: shard.worker_threads,
                    created_at: shard.created_at,
                    active_generations,
                    assigned_instances,
                    pressure_state: self
                        .inner
                        .pressure_state
                        .get(&shard.shard_id)
                        .map_or(HighPriorityShardPressureState::Unknown, |state| *state),
                }
            })
            .collect();
        snapshots.sort_by_key(|snapshot| snapshot.shard_id);
        snapshots
    }

    pub(super) fn select_generation(
        &self,
        service_instance_id: ServiceInstanceId,
        generation: u64,
    ) -> Option<(HighPriorityShardHandle, HighPriorityPlacementDecision)> {
        let shards = self.shards.read().unwrap_or_else(|err| err.into_inner());
        let shard =
            shards
                .iter()
                .min_by_key(|shard| {
                    let active = self
                        .active_generations
                        .iter()
                        .filter(|entry| *entry.value() == shard.shard_id)
                        .count();
                    let pressure_penalty =
                        usize::from(self.inner.pressure_state.get(&shard.shard_id).is_some_and(
                            |state| *state == HighPriorityShardPressureState::Pressured,
                        ));
                    (pressure_penalty, active, shard.shard_id)
                })
                .cloned()?;
        drop(shards);

        self.assignments.insert(service_instance_id, shard.shard_id);
        self.active_generations
            .insert((service_instance_id, generation), shard.shard_id);
        let decision = HighPriorityPlacementDecision {
            shard_id: Some(shard.shard_id),
            kind: HighPriorityPlacementDecisionKind::LeastLoaded,
            reason: HighPriorityPlacementReason::LeastLoadedShard,
        };
        Some((shard, decision))
    }

    pub(super) fn release_generation(
        &self,
        service_instance_id: ServiceInstanceId,
        generation: u64,
    ) {
        self.active_generations
            .remove(&(service_instance_id, generation));
        self.pending_rollovers
            .remove(&(service_instance_id, generation));
    }

    pub(super) fn remove_service_instance(&self, service_instance_id: ServiceInstanceId) {
        self.assignments.remove(&service_instance_id);
        self.remove_service_instance_generations(service_instance_id);
    }

    pub(super) fn remove_service_instance_generations(
        &self,
        service_instance_id: ServiceInstanceId,
    ) {
        self.active_generations
            .retain(|(active_service_instance_id, _), _| {
                *active_service_instance_id != service_instance_id
            });
        self.pending_rollovers
            .retain(|(pending_service_instance_id, _), _| {
                *pending_service_instance_id != service_instance_id
            });
    }

    pub(super) fn request_rollover(
        &self,
        service_instance_id: ServiceInstanceId,
        generation: u64,
        target_shard_id: HighPriorityShardId,
    ) -> bool {
        self.pending_rollovers
            .insert((service_instance_id, generation), target_shard_id)
            .is_none()
    }

    pub(super) fn active_on_shard(
        &self,
        shard_id: HighPriorityShardId,
    ) -> Vec<(ServiceInstanceId, u64)> {
        let mut active: Vec<_> = self
            .active_generations
            .iter()
            .filter_map(|entry| (*entry.value() == shard_id).then_some(*entry.key()))
            .collect();
        active.sort_unstable();
        active
    }

    pub(super) fn record_pressure(
        &self,
        shard_id: HighPriorityShardId,
        pressure: HighPriorityShardPressureState,
    ) {
        self.inner.pressure_state.insert(shard_id, pressure);
    }
}

pub(super) struct HighPriorityRuntimePool {
    policy: HighPriorityRuntimeControl,
    capacity_plan: HighPriorityCapacityPlan,
    available_parallelism: Option<NonZeroUsize>,
    state: Arc<HighPriorityRuntimePoolState>,
    runtimes: Vec<Runtime>,
    next_shard_id: u64,
    total_worker_threads: usize,
    consecutive_pressure_windows: u32,
    last_scale_at: Option<Instant>,
    last_rollover_at: Option<Instant>,
}

impl HighPriorityRuntimePool {
    pub(super) fn new(
        policy: HighPriorityRuntimeControl,
        capacity_plan: HighPriorityCapacityPlan,
    ) -> Self {
        Self::new_with_parallelism(
            policy,
            capacity_plan,
            std::thread::available_parallelism().ok(),
        )
    }

    pub(super) fn new_with_parallelism(
        policy: HighPriorityRuntimeControl,
        capacity_plan: HighPriorityCapacityPlan,
        available_parallelism: Option<NonZeroUsize>,
    ) -> Self {
        Self {
            policy,
            capacity_plan,
            available_parallelism,
            state: Arc::new(HighPriorityRuntimePoolState::default()),
            runtimes: Vec::new(),
            next_shard_id: 0,
            total_worker_threads: 0,
            consecutive_pressure_windows: 0,
            last_scale_at: None,
            last_rollover_at: None,
        }
    }

    pub(super) fn state(&self) -> Arc<HighPriorityRuntimePoolState> {
        self.state.clone()
    }

    pub(super) fn shard_handles(&self) -> Vec<HighPriorityShardHandle> {
        self.state
            .shards
            .read()
            .unwrap_or_else(|err| err.into_inner())
            .clone()
    }

    pub(super) fn shard_handle(
        &self,
        shard_id: HighPriorityShardId,
    ) -> Option<HighPriorityShardHandle> {
        self.state
            .shards
            .read()
            .unwrap_or_else(|err| err.into_inner())
            .iter()
            .find(|shard| shard.shard_id == shard_id)
            .cloned()
    }

    pub(super) fn policy(&self) -> HighPriorityRuntimeControl {
        self.policy
    }

    pub(super) fn total_worker_threads(&self) -> usize {
        self.total_worker_threads
    }

    pub(super) fn max_worker_threads(&self) -> usize {
        self.policy
            .max_worker_threads()
            .or(self.available_parallelism)
            .map_or(1, NonZeroUsize::get)
            .max(1)
    }

    pub(super) fn is_empty(&self) -> bool {
        self.runtimes.is_empty()
    }

    pub(super) fn prepare_initial_runtime(&mut self) -> std::io::Result<Option<Handle>> {
        let Some(worker_count) = self.capacity_plan.worker_count() else {
            return Ok(None);
        };
        if self.is_empty() {
            let capped = worker_count.get().min(self.max_worker_threads()).max(1);
            self.create_shard(capped)?;
        }
        Ok(self
            .state
            .shards
            .read()
            .unwrap_or_else(|err| err.into_inner())
            .first()
            .map(|shard| shard.handle.clone()))
    }

    pub(super) fn scale_out(
        &mut self,
        now: Instant,
    ) -> std::io::Result<Option<HighPriorityShardId>> {
        if self
            .last_scale_at
            .and_then(|last| now.checked_duration_since(last))
            .is_some_and(|elapsed| elapsed < self.policy.scale_cooldown())
        {
            return Ok(None);
        }
        let remaining = self
            .max_worker_threads()
            .saturating_sub(self.total_worker_threads);
        if remaining == 0 {
            return Ok(None);
        }
        let worker_threads = self
            .policy
            .scale_step_worker_threads()
            .get()
            .min(remaining)
            .max(1);
        let shard_id = self.create_shard(worker_threads)?;
        self.last_scale_at = Some(now);
        Ok(Some(shard_id))
    }

    pub(super) fn record_pressure_window(&mut self, pressured: bool) -> bool {
        if pressured {
            self.consecutive_pressure_windows = self.consecutive_pressure_windows.saturating_add(1);
        } else {
            self.consecutive_pressure_windows = 0;
        }
        self.consecutive_pressure_windows >= self.policy.pressure_windows()
    }

    pub(super) fn rollover_allowed(&mut self, now: Instant) -> bool {
        if self.policy.max_rollovers_per_window() == 0 {
            return false;
        }
        if self
            .last_rollover_at
            .and_then(|last| now.checked_duration_since(last))
            .is_some_and(|elapsed| elapsed < self.policy.rollover_cooldown())
        {
            return false;
        }
        true
    }

    pub(super) fn record_rollover_batch(&mut self, now: Instant) {
        self.last_rollover_at = Some(now);
    }

    pub(super) fn snapshot(&self) -> Vec<HighPriorityRuntimeShardSnapshot> {
        self.state.snapshot()
    }

    fn create_shard(&mut self, worker_threads: usize) -> std::io::Result<HighPriorityShardId> {
        let shard_id = HighPriorityShardId(self.next_shard_id);
        self.next_shard_id = self.next_shard_id.saturating_add(1);
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .worker_threads(worker_threads)
            .thread_name(format!("svc-high-priority-{}", shard_id.0))
            .build()?;
        let handle = runtime.handle().clone();
        self.runtimes.push(runtime);
        self.total_worker_threads = self.total_worker_threads.saturating_add(worker_threads);
        self.state
            .record_pressure(shard_id, HighPriorityShardPressureState::Unknown);
        self.state
            .shards
            .write()
            .unwrap_or_else(|err| err.into_inner())
            .push(HighPriorityShardHandle {
                shard_id,
                worker_threads,
                created_at: Utc::now(),
                handle,
            });
        Ok(shard_id)
    }

    pub(super) fn shutdown(&mut self) {
        let runtimes = std::mem::take(&mut self.runtimes);
        self.state
            .shards
            .write()
            .unwrap_or_else(|err| err.into_inner())
            .clear();
        self.total_worker_threads = 0;
        for runtime in runtimes {
            if let Err(panic) = std::thread::spawn(move || drop(runtime)).join() {
                tracing::error!(
                    ?panic,
                    "High-priority runtime shard shutdown thread panicked"
                );
            }
        }
    }

    pub(super) fn shutdown_detached(&mut self) {
        let runtimes = std::mem::take(&mut self.runtimes);
        self.state
            .shards
            .write()
            .unwrap_or_else(|err| err.into_inner())
            .clear();
        self.total_worker_threads = 0;
        for runtime in runtimes {
            let _ = std::thread::spawn(move || drop(runtime));
        }
    }
}

pub(super) fn observation_has_pressure(
    completed: u64,
    avg_drift_ms: u64,
    policy: HighPriorityRuntimeControl,
) -> bool {
    completed >= policy.minimum_completed_samples() && avg_drift_ms >= policy.high_avg_drift_ms()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn plan(entries: usize) -> HighPriorityCapacityPlan {
        HighPriorityCapacityPlan::from_entry_count(entries, NonZeroUsize::new(8))
    }

    #[test]
    fn initial_pool_creates_static_capacity_shard() {
        let mut pool = HighPriorityRuntimePool::new_with_parallelism(
            HighPriorityRuntimeControl::for_testing(),
            plan(2),
            NonZeroUsize::new(8),
        );

        let handle = pool
            .prepare_initial_runtime()
            .expect("runtime should build");

        assert!(handle.is_some());
        let shards = pool.snapshot();
        assert_eq!(shards.len(), 1);
        assert_eq!(shards[0].worker_threads, 2);
        pool.shutdown();
    }

    #[test]
    fn scale_out_respects_max_capacity() {
        let mut pool = HighPriorityRuntimePool::new_with_parallelism(
            HighPriorityRuntimeControl::for_testing(),
            plan(1),
            NonZeroUsize::new(2),
        );
        pool.prepare_initial_runtime()
            .expect("runtime should build");

        assert_eq!(
            pool.scale_out(Instant::now())
                .expect("scale should not fail"),
            Some(HighPriorityShardId(1))
        );
        assert_eq!(
            pool.scale_out(Instant::now() + Duration::from_secs(10))
                .expect("scale should not fail"),
            None
        );
        assert_eq!(pool.snapshot().len(), 2);
        assert_eq!(pool.total_worker_threads(), 2);
        pool.shutdown();
    }

    #[test]
    fn placement_uses_least_loaded_non_pressured_shard() {
        let mut pool = HighPriorityRuntimePool::new_with_parallelism(
            HighPriorityRuntimeControl::for_testing(),
            plan(1),
            NonZeroUsize::new(4),
        );
        pool.prepare_initial_runtime()
            .expect("runtime should build");
        pool.scale_out(Instant::now() + Duration::from_secs(10))
            .expect("scale should not fail");
        pool.state.record_pressure(
            HighPriorityShardId(0),
            HighPriorityShardPressureState::Pressured,
        );

        let service_id = ServiceInstanceId::new(uuid::Uuid::from_u128(10));
        let (_, decision) = pool
            .state()
            .select_generation(service_id, 1)
            .expect("placement should select a shard");

        assert_eq!(decision.shard_id, Some(HighPriorityShardId(1)));
        pool.state().release_generation(service_id, 1);
        pool.shutdown();
    }

    #[test]
    fn pressure_windows_require_default_hysteresis() {
        let mut pool = HighPriorityRuntimePool::new_with_parallelism(
            HighPriorityRuntimeControl::automatic(),
            plan(1),
            NonZeroUsize::new(4),
        );

        assert!(!pool.record_pressure_window(true));
        assert!(pool.record_pressure_window(true));
        assert!(!pool.record_pressure_window(false));
    }

    #[tokio::test]
    async fn state_removes_instance_assignments_active_generations_and_pending_rollovers() {
        let state = HighPriorityRuntimePoolState::for_test_current_runtime();
        let service_id = ServiceInstanceId::new(uuid::Uuid::from_u128(44));

        state
            .select_generation(service_id, 1)
            .expect("generation should be placed");
        assert!(state.request_rollover(service_id, 1, HighPriorityShardId(0)));

        state.remove_service_instance(service_id);

        let snapshot = state.snapshot();
        assert_eq!(snapshot[0].assigned_instances, 0);
        assert_eq!(snapshot[0].active_generations, 0);
        assert!(state.request_rollover(service_id, 1, HighPriorityShardId(0)));
    }

    #[tokio::test]
    async fn state_deduplicates_rollover_requests_until_generation_releases() {
        let state = HighPriorityRuntimePoolState::for_test_current_runtime();
        let service_id = ServiceInstanceId::new(uuid::Uuid::from_u128(45));

        state
            .select_generation(service_id, 1)
            .expect("generation should be placed");

        assert!(state.request_rollover(service_id, 1, HighPriorityShardId(0)));
        assert!(!state.request_rollover(service_id, 1, HighPriorityShardId(0)));
        state.release_generation(service_id, 1);
        assert!(state.request_rollover(service_id, 1, HighPriorityShardId(0)));
    }
}
