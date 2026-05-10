use dashmap::DashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

use crate::models::{ServiceId, ServiceScheduling};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum RuntimeLane {
    Control,
    Standard,
    HighPriority,
    Isolated,
}

impl From<ServiceScheduling> for RuntimeLane {
    fn from(value: ServiceScheduling) -> Self {
        match value {
            ServiceScheduling::Standard => Self::Standard,
            ServiceScheduling::HighPriority => Self::HighPriority,
            ServiceScheduling::Isolated => Self::Isolated,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SleepObservationSource {
    ServiceSleep,
    RuntimeProbe,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SleepExitReason {
    Completed,
    Reload,
    Shutdown,
    ProbeCancelled,
}

impl SleepExitReason {
    fn completed(self) -> bool {
        matches!(self, Self::Completed)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GenerationExitKind {
    NormalExit,
    RecoverableError,
    Panic,
    FatalServiceError,
    ProviderInitError,
    Reload,
    Shutdown,
    IsolatedStartupFailure,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SleepObservation {
    pub source: SleepObservationSource,
    pub reason: SleepExitReason,
    pub requested: Duration,
    pub elapsed: Duration,
    pub drift: Duration,
}

const RUNTIME_PROBE_INTERVAL: Duration = Duration::from_millis(250);
const RETAINED_GENERATIONS_PER_SERVICE: usize = 1024;

pub(crate) async fn run_lane_runtime_probe(
    diagnostics: Arc<DiagnosticsStore>,
    lane: RuntimeLane,
    token: CancellationToken,
) {
    loop {
        let start = Instant::now();
        tokio::select! {
            _ = tokio::time::sleep(RUNTIME_PROBE_INTERVAL) => {
                diagnostics.record_lane_observation(
                    lane,
                    runtime_probe_observation(SleepExitReason::Completed, start),
                );
            }
            _ = token.cancelled() => {
                diagnostics.record_lane_observation(
                    lane,
                    runtime_probe_observation(SleepExitReason::ProbeCancelled, start),
                );
                break;
            }
        }
    }
}

pub(crate) async fn run_generation_runtime_probe(
    diagnostics: GenerationDiagnosticsHandle,
    token: CancellationToken,
) {
    loop {
        let start = Instant::now();
        tokio::select! {
            _ = tokio::time::sleep(RUNTIME_PROBE_INTERVAL) => {
                diagnostics.record_sleep_observation(runtime_probe_observation(
                    SleepExitReason::Completed,
                    start,
                ));
            }
            _ = token.cancelled() => {
                diagnostics.record_sleep_observation(runtime_probe_observation(
                    SleepExitReason::ProbeCancelled,
                    start,
                ));
                break;
            }
        }
    }
}

fn runtime_probe_observation(reason: SleepExitReason, start: Instant) -> SleepObservation {
    let elapsed = start.elapsed();
    SleepObservation {
        source: SleepObservationSource::RuntimeProbe,
        reason,
        requested: RUNTIME_PROBE_INTERVAL,
        elapsed,
        drift: if reason.completed() {
            elapsed.saturating_sub(RUNTIME_PROBE_INTERVAL)
        } else {
            Duration::ZERO
        },
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ObservationStatsSnapshot {
    pub completed: u64,
    pub interrupted: u64,
    pub total_requested_ms: u64,
    pub total_elapsed_ms: u64,
    pub total_drift_ms: u64,
    pub max_drift_ms: u64,
    pub last_drift_ms: u64,
    pub avg_drift_ms: u64,
}

#[derive(Default)]
struct ObservationStats {
    completed: AtomicU64,
    interrupted: AtomicU64,
    total_requested_ns: AtomicU64,
    total_elapsed_ns: AtomicU64,
    total_drift_ns: AtomicU64,
    max_drift_ns: AtomicU64,
    last_drift_ns: AtomicU64,
}

impl ObservationStats {
    fn record(
        &self,
        reason: SleepExitReason,
        requested: Duration,
        elapsed: Duration,
        drift: Duration,
    ) {
        let requested_ns = duration_nanos(requested);
        let elapsed_ns = duration_nanos(elapsed);

        self.total_requested_ns
            .fetch_add(requested_ns, Ordering::Relaxed);
        self.total_elapsed_ns
            .fetch_add(elapsed_ns, Ordering::Relaxed);

        if reason.completed() {
            let drift_ns = duration_nanos(drift);
            self.completed.fetch_add(1, Ordering::Relaxed);
            self.total_drift_ns.fetch_add(drift_ns, Ordering::Relaxed);
            self.last_drift_ns.store(drift_ns, Ordering::Relaxed);
            update_max(&self.max_drift_ns, drift_ns);
        } else {
            self.interrupted.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn snapshot(&self) -> ObservationStatsSnapshot {
        let completed = self.completed.load(Ordering::Relaxed);
        let total_drift_ns = self.total_drift_ns.load(Ordering::Relaxed);

        ObservationStatsSnapshot {
            completed,
            interrupted: self.interrupted.load(Ordering::Relaxed),
            total_requested_ms: nanos_to_millis(self.total_requested_ns.load(Ordering::Relaxed)),
            total_elapsed_ms: nanos_to_millis(self.total_elapsed_ns.load(Ordering::Relaxed)),
            total_drift_ms: nanos_to_millis(total_drift_ns),
            max_drift_ms: nanos_to_millis(self.max_drift_ns.load(Ordering::Relaxed)),
            last_drift_ms: nanos_to_millis(self.last_drift_ns.load(Ordering::Relaxed)),
            avg_drift_ms: total_drift_ns
                .checked_div(completed)
                .map_or(0, nanos_to_millis),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LifecycleStatsSnapshot {
    pub reload_requested: u64,
    pub reload_exit: u64,
    pub restart: u64,
    pub backoff_restart: u64,
    pub rate_limited_restart: u64,
    pub terminated: u64,
    pub normal_exit: u64,
    pub recoverable_error: u64,
    pub panic: u64,
    pub fatal_service_error: u64,
    pub provider_init_error: u64,
    pub shutdown: u64,
    pub isolated_startup_failure: u64,
    pub last_policy_delay_ms: u64,
    pub last_effective_restart_delay_ms: u64,
    pub last_exit_kind: Option<GenerationExitKind>,
}

#[derive(Default)]
struct LifecycleStats {
    reload_requested: AtomicU64,
    reload_exit: AtomicU64,
    restart: AtomicU64,
    backoff_restart: AtomicU64,
    rate_limited_restart: AtomicU64,
    terminated: AtomicU64,
    normal_exit: AtomicU64,
    recoverable_error: AtomicU64,
    panic: AtomicU64,
    fatal_service_error: AtomicU64,
    provider_init_error: AtomicU64,
    shutdown: AtomicU64,
    isolated_startup_failure: AtomicU64,
    last_policy_delay_ms: AtomicU64,
    last_effective_restart_delay_ms: AtomicU64,
    last_exit_kind: Mutex<Option<GenerationExitKind>>,
}

impl LifecycleStats {
    fn record_reload_requested(&self) {
        self.reload_requested.fetch_add(1, Ordering::Relaxed);
    }

    fn record_restart(
        &self,
        backoff: bool,
        policy_delay: Duration,
        effective_delay: Duration,
        rate_limited: bool,
    ) {
        self.restart.fetch_add(1, Ordering::Relaxed);
        if backoff {
            self.backoff_restart.fetch_add(1, Ordering::Relaxed);
        }
        if rate_limited {
            self.rate_limited_restart.fetch_add(1, Ordering::Relaxed);
        }
        self.last_policy_delay_ms
            .store(duration_millis(policy_delay), Ordering::Relaxed);
        self.last_effective_restart_delay_ms
            .store(duration_millis(effective_delay), Ordering::Relaxed);
    }

    fn record_terminated(&self) {
        self.terminated.fetch_add(1, Ordering::Relaxed);
    }

    fn record_exit(&self, kind: GenerationExitKind) {
        match kind {
            GenerationExitKind::NormalExit => &self.normal_exit,
            GenerationExitKind::RecoverableError => &self.recoverable_error,
            GenerationExitKind::Panic => &self.panic,
            GenerationExitKind::FatalServiceError => &self.fatal_service_error,
            GenerationExitKind::ProviderInitError => &self.provider_init_error,
            GenerationExitKind::Reload => &self.reload_exit,
            GenerationExitKind::Shutdown => &self.shutdown,
            GenerationExitKind::IsolatedStartupFailure => &self.isolated_startup_failure,
        }
        .fetch_add(1, Ordering::Relaxed);

        *lock_or_recover(&self.last_exit_kind) = Some(kind);
    }

    fn snapshot(&self) -> LifecycleStatsSnapshot {
        LifecycleStatsSnapshot {
            reload_requested: self.reload_requested.load(Ordering::Relaxed),
            reload_exit: self.reload_exit.load(Ordering::Relaxed),
            restart: self.restart.load(Ordering::Relaxed),
            backoff_restart: self.backoff_restart.load(Ordering::Relaxed),
            rate_limited_restart: self.rate_limited_restart.load(Ordering::Relaxed),
            terminated: self.terminated.load(Ordering::Relaxed),
            normal_exit: self.normal_exit.load(Ordering::Relaxed),
            recoverable_error: self.recoverable_error.load(Ordering::Relaxed),
            panic: self.panic.load(Ordering::Relaxed),
            fatal_service_error: self.fatal_service_error.load(Ordering::Relaxed),
            provider_init_error: self.provider_init_error.load(Ordering::Relaxed),
            shutdown: self.shutdown.load(Ordering::Relaxed),
            isolated_startup_failure: self.isolated_startup_failure.load(Ordering::Relaxed),
            last_policy_delay_ms: self.last_policy_delay_ms.load(Ordering::Relaxed),
            last_effective_restart_delay_ms: self
                .last_effective_restart_delay_ms
                .load(Ordering::Relaxed),
            last_exit_kind: *lock_or_recover(&self.last_exit_kind),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DiagnosticsAggregateSnapshot {
    pub service_sleep: ObservationStatsSnapshot,
    pub runtime_probe: ObservationStatsSnapshot,
    pub lifecycle: LifecycleStatsSnapshot,
}

#[derive(Default)]
struct DiagnosticsAggregate {
    service_sleep: ObservationStats,
    runtime_probe: ObservationStats,
    lifecycle: LifecycleStats,
}

impl DiagnosticsAggregate {
    fn record_observation(&self, observation: SleepObservation) {
        match observation.source {
            SleepObservationSource::ServiceSleep => &self.service_sleep,
            SleepObservationSource::RuntimeProbe => &self.runtime_probe,
        }
        .record(
            observation.reason,
            observation.requested,
            observation.elapsed,
            observation.drift,
        );
    }

    fn snapshot(&self) -> DiagnosticsAggregateSnapshot {
        DiagnosticsAggregateSnapshot {
            service_sleep: self.service_sleep.snapshot(),
            runtime_probe: self.runtime_probe.snapshot(),
            lifecycle: self.lifecycle.snapshot(),
        }
    }
}

struct ServiceDiagnostics {
    service_id: ServiceId,
    service_name: &'static str,
    current_generation: AtomicU64,
    runtime_lane: Mutex<RuntimeLane>,
    aggregate: DiagnosticsAggregate,
}

impl ServiceDiagnostics {
    fn new(service_id: ServiceId, service_name: &'static str, lane: RuntimeLane) -> Self {
        Self {
            service_id,
            service_name,
            current_generation: AtomicU64::new(0),
            runtime_lane: Mutex::new(lane),
            aggregate: DiagnosticsAggregate::default(),
        }
    }

    fn update_generation(&self, generation: u64, lane: RuntimeLane) {
        self.current_generation.store(generation, Ordering::Relaxed);
        *lock_or_recover(&self.runtime_lane) = lane;
    }

    fn snapshot(&self) -> ServiceDiagnosticsSnapshot {
        ServiceDiagnosticsSnapshot {
            service_id: self.service_id,
            service_name: self.service_name,
            current_generation: self.current_generation.load(Ordering::Relaxed),
            runtime_lane: *lock_or_recover(&self.runtime_lane),
            aggregate: self.aggregate.snapshot(),
        }
    }
}

struct GenerationDiagnostics {
    service_id: ServiceId,
    service_name: &'static str,
    generation: u64,
    runtime_lane: RuntimeLane,
    aggregate: DiagnosticsAggregate,
}

impl GenerationDiagnostics {
    fn new(
        service_id: ServiceId,
        service_name: &'static str,
        generation: u64,
        runtime_lane: RuntimeLane,
    ) -> Self {
        Self {
            service_id,
            service_name,
            generation,
            runtime_lane,
            aggregate: DiagnosticsAggregate::default(),
        }
    }

    fn snapshot(&self) -> GenerationDiagnosticsSnapshot {
        GenerationDiagnosticsSnapshot {
            service_id: self.service_id,
            service_name: self.service_name,
            generation: self.generation,
            runtime_lane: self.runtime_lane,
            aggregate: self.aggregate.snapshot(),
        }
    }
}

struct LaneDiagnostics {
    runtime_lane: RuntimeLane,
    aggregate: DiagnosticsAggregate,
}

impl LaneDiagnostics {
    fn new(runtime_lane: RuntimeLane) -> Self {
        Self {
            runtime_lane,
            aggregate: DiagnosticsAggregate::default(),
        }
    }

    fn snapshot(&self) -> RuntimeLaneSnapshot {
        RuntimeLaneSnapshot {
            runtime_lane: self.runtime_lane,
            aggregate: self.aggregate.snapshot(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ServiceDiagnosticsSnapshot {
    pub service_id: ServiceId,
    pub service_name: &'static str,
    pub current_generation: u64,
    pub runtime_lane: RuntimeLane,
    pub aggregate: DiagnosticsAggregateSnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GenerationDiagnosticsSnapshot {
    pub service_id: ServiceId,
    pub service_name: &'static str,
    pub generation: u64,
    pub runtime_lane: RuntimeLane,
    pub aggregate: DiagnosticsAggregateSnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimeLaneSnapshot {
    pub runtime_lane: RuntimeLane,
    pub aggregate: DiagnosticsAggregateSnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DiagnosticsSnapshot {
    pub services: Vec<ServiceDiagnosticsSnapshot>,
    pub generations: Vec<GenerationDiagnosticsSnapshot>,
    pub lanes: Vec<RuntimeLaneSnapshot>,
}

#[derive(Clone)]
pub(crate) struct GenerationDiagnosticsHandle {
    service: Arc<ServiceDiagnostics>,
    generation: Arc<GenerationDiagnostics>,
    lane: Arc<LaneDiagnostics>,
}

impl GenerationDiagnosticsHandle {
    pub(crate) fn runtime_lane(&self) -> RuntimeLane {
        self.generation.runtime_lane
    }

    pub(crate) fn record_sleep_observation(&self, observation: SleepObservation) {
        self.generation.aggregate.record_observation(observation);
        self.service.aggregate.record_observation(observation);
        self.lane.aggregate.record_observation(observation);
    }

    pub(crate) fn record_exit(&self, kind: GenerationExitKind) {
        self.generation.aggregate.lifecycle.record_exit(kind);
        self.service.aggregate.lifecycle.record_exit(kind);
        self.lane.aggregate.lifecycle.record_exit(kind);
    }

    pub(crate) fn record_reload_requested(&self) {
        self.generation
            .aggregate
            .lifecycle
            .record_reload_requested();
        self.service.aggregate.lifecycle.record_reload_requested();
        self.lane.aggregate.lifecycle.record_reload_requested();
    }

    pub(crate) fn record_restart(
        &self,
        backoff: bool,
        policy_delay: Duration,
        effective_delay: Duration,
        rate_limited: bool,
    ) {
        self.generation.aggregate.lifecycle.record_restart(
            backoff,
            policy_delay,
            effective_delay,
            rate_limited,
        );
        self.service.aggregate.lifecycle.record_restart(
            backoff,
            policy_delay,
            effective_delay,
            rate_limited,
        );
        self.lane.aggregate.lifecycle.record_restart(
            backoff,
            policy_delay,
            effective_delay,
            rate_limited,
        );
    }

    pub(crate) fn record_terminated(&self) {
        self.generation.aggregate.lifecycle.record_terminated();
        self.service.aggregate.lifecycle.record_terminated();
        self.lane.aggregate.lifecycle.record_terminated();
    }

    pub(crate) fn snapshot(&self) -> GenerationDiagnosticsSnapshot {
        self.generation.snapshot()
    }
}

pub(crate) struct DiagnosticsStore {
    services: DashMap<ServiceId, Arc<ServiceDiagnostics>>,
    generations: DashMap<(ServiceId, u64), Arc<GenerationDiagnostics>>,
    control: Arc<LaneDiagnostics>,
    standard: Arc<LaneDiagnostics>,
    high_priority: Arc<LaneDiagnostics>,
    isolated: Arc<LaneDiagnostics>,
}

impl Default for DiagnosticsStore {
    fn default() -> Self {
        Self {
            services: DashMap::new(),
            generations: DashMap::new(),
            control: Arc::new(LaneDiagnostics::new(RuntimeLane::Control)),
            standard: Arc::new(LaneDiagnostics::new(RuntimeLane::Standard)),
            high_priority: Arc::new(LaneDiagnostics::new(RuntimeLane::HighPriority)),
            isolated: Arc::new(LaneDiagnostics::new(RuntimeLane::Isolated)),
        }
    }
}

impl DiagnosticsStore {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn register_generation(
        &self,
        service_id: ServiceId,
        service_name: &'static str,
        generation: u64,
        lane: RuntimeLane,
    ) -> GenerationDiagnosticsHandle {
        let service = self
            .services
            .entry(service_id)
            .or_insert_with(|| Arc::new(ServiceDiagnostics::new(service_id, service_name, lane)))
            .clone();
        service.update_generation(generation, lane);

        let generation_diagnostics = Arc::new(GenerationDiagnostics::new(
            service_id,
            service_name,
            generation,
            lane,
        ));
        self.generations
            .insert((service_id, generation), generation_diagnostics.clone());
        self.retain_recent_generations(service_id);

        GenerationDiagnosticsHandle {
            service,
            generation: generation_diagnostics,
            lane: self.lane_diagnostics(lane),
        }
    }

    pub(crate) fn record_lane_observation(&self, lane: RuntimeLane, observation: SleepObservation) {
        self.lane_diagnostics(lane)
            .aggregate
            .record_observation(observation);
    }

    #[cfg(test)]
    pub(crate) fn service_snapshot(
        &self,
        service_id: ServiceId,
    ) -> Option<ServiceDiagnosticsSnapshot> {
        self.services
            .get(&service_id)
            .map(|service| service.snapshot())
    }

    #[cfg(test)]
    pub(crate) fn generation_snapshot(
        &self,
        service_id: ServiceId,
        generation: u64,
    ) -> Option<GenerationDiagnosticsSnapshot> {
        self.generations
            .get(&(service_id, generation))
            .map(|generation| generation.snapshot())
    }

    #[cfg(test)]
    pub(crate) fn lane_snapshot(&self, lane: RuntimeLane) -> RuntimeLaneSnapshot {
        self.lane_diagnostics(lane).snapshot()
    }

    pub(crate) fn snapshot(&self) -> DiagnosticsSnapshot {
        let mut services: Vec<_> = self
            .services
            .iter()
            .map(|service| service.value().snapshot())
            .collect();
        services.sort_by_key(|snapshot| snapshot.service_id);

        let mut generations: Vec<_> = self
            .generations
            .iter()
            .map(|generation| generation.value().snapshot())
            .collect();
        generations.sort_by_key(|snapshot| (snapshot.service_id, snapshot.generation));

        DiagnosticsSnapshot {
            services,
            generations,
            lanes: vec![
                self.control.snapshot(),
                self.standard.snapshot(),
                self.high_priority.snapshot(),
                self.isolated.snapshot(),
            ],
        }
    }

    fn lane_diagnostics(&self, lane: RuntimeLane) -> Arc<LaneDiagnostics> {
        match lane {
            RuntimeLane::Control => self.control.clone(),
            RuntimeLane::Standard => self.standard.clone(),
            RuntimeLane::HighPriority => self.high_priority.clone(),
            RuntimeLane::Isolated => self.isolated.clone(),
        }
    }

    fn retain_recent_generations(&self, service_id: ServiceId) {
        let mut generations: Vec<_> = self
            .generations
            .iter()
            .filter_map(|entry| {
                let (entry_service_id, generation) = *entry.key();
                (entry_service_id == service_id).then_some(generation)
            })
            .collect();

        if generations.len() <= RETAINED_GENERATIONS_PER_SERVICE {
            return;
        }

        generations.sort_unstable();
        let evict_count = generations.len() - RETAINED_GENERATIONS_PER_SERVICE;
        for generation in generations.into_iter().take(evict_count) {
            self.generations.remove(&(service_id, generation));
        }
    }
}

fn duration_nanos(duration: Duration) -> u64 {
    duration.as_nanos().min(u128::from(u64::MAX)) as u64
}

fn duration_millis(duration: Duration) -> u64 {
    duration.as_millis().min(u128::from(u64::MAX)) as u64
}

fn nanos_to_millis(nanos: u64) -> u64 {
    nanos / 1_000_000
}

fn lock_or_recover<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn update_max(target: &AtomicU64, value: u64) {
    let mut current = target.load(Ordering::Relaxed);
    while value > current {
        match target.compare_exchange_weak(current, value, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => break,
            Err(next) => current = next,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_lane_maps_from_service_scheduling() {
        assert_eq!(
            RuntimeLane::from(ServiceScheduling::Standard),
            RuntimeLane::Standard
        );
        assert_eq!(
            RuntimeLane::from(ServiceScheduling::HighPriority),
            RuntimeLane::HighPriority
        );
        assert_eq!(
            RuntimeLane::from(ServiceScheduling::Isolated),
            RuntimeLane::Isolated
        );
        assert_ne!(
            RuntimeLane::from(ServiceScheduling::Standard),
            RuntimeLane::Control
        );
    }

    fn completed_observation(
        source: SleepObservationSource,
        requested: Duration,
        elapsed: Duration,
    ) -> SleepObservation {
        SleepObservation {
            source,
            reason: SleepExitReason::Completed,
            requested,
            elapsed,
            drift: elapsed.saturating_sub(requested),
        }
    }

    fn record_standard_lane_probe(store: &DiagnosticsStore, elapsed: Duration) {
        store.record_lane_observation(
            RuntimeLane::Standard,
            completed_observation(
                SleepObservationSource::RuntimeProbe,
                Duration::from_millis(250),
                elapsed,
            ),
        );
    }

    fn interpretation_labels(
        interpretations: &[crate::models::DiagnosticInterpretation],
    ) -> Vec<crate::models::DiagnosticInterpretationLabel> {
        interpretations
            .iter()
            .map(|interpretation| interpretation.label)
            .collect()
    }

    #[test]
    fn service_sleep_observation_updates_generation_service_and_lane() {
        let store = DiagnosticsStore::new();
        let handle =
            store.register_generation(ServiceId::new(7), "worker", 3, RuntimeLane::HighPriority);

        handle.record_sleep_observation(SleepObservation {
            source: SleepObservationSource::ServiceSleep,
            reason: SleepExitReason::Completed,
            requested: Duration::from_millis(10),
            elapsed: Duration::from_millis(15),
            drift: Duration::from_millis(5),
        });

        let generation = handle.snapshot();
        assert_eq!(generation.generation, 3);
        assert_eq!(generation.aggregate.service_sleep.completed, 1);
        assert_eq!(generation.aggregate.service_sleep.total_drift_ms, 5);
        assert_eq!(generation.aggregate.service_sleep.max_drift_ms, 5);

        let service = store.service_snapshot(ServiceId::new(7)).unwrap();
        assert_eq!(service.current_generation, 3);
        assert_eq!(service.runtime_lane, RuntimeLane::HighPriority);
        assert_eq!(service.aggregate.service_sleep.completed, 1);

        let lane = store.lane_snapshot(RuntimeLane::HighPriority);
        assert_eq!(lane.aggregate.service_sleep.completed, 1);
        assert_eq!(lane.aggregate.service_sleep.avg_drift_ms, 5);
    }

    #[test]
    fn interrupted_sleep_does_not_add_drift() {
        let store = DiagnosticsStore::new();
        let handle =
            store.register_generation(ServiceId::new(1), "reloading", 1, RuntimeLane::Standard);

        handle.record_sleep_observation(SleepObservation {
            source: SleepObservationSource::ServiceSleep,
            reason: SleepExitReason::Reload,
            requested: Duration::from_secs(10),
            elapsed: Duration::from_millis(50),
            drift: Duration::from_millis(0),
        });

        let snapshot = handle.snapshot();
        assert_eq!(snapshot.aggregate.service_sleep.completed, 0);
        assert_eq!(snapshot.aggregate.service_sleep.interrupted, 1);
        assert_eq!(snapshot.aggregate.service_sleep.total_drift_ms, 0);
        assert_eq!(snapshot.aggregate.service_sleep.max_drift_ms, 0);
    }

    #[test]
    fn lifecycle_classification_updates_counters() {
        let store = DiagnosticsStore::new();
        let handle =
            store.register_generation(ServiceId::new(2), "isolated", 9, RuntimeLane::Isolated);

        handle.record_reload_requested();
        handle.record_exit(GenerationExitKind::IsolatedStartupFailure);
        handle.record_restart(
            true,
            Duration::from_millis(250),
            Duration::from_millis(500),
            true,
        );
        handle.record_terminated();

        let lifecycle = handle.snapshot().aggregate.lifecycle;
        assert_eq!(lifecycle.reload_requested, 1);
        assert_eq!(lifecycle.isolated_startup_failure, 1);
        assert_eq!(lifecycle.restart, 1);
        assert_eq!(lifecycle.backoff_restart, 1);
        assert_eq!(lifecycle.rate_limited_restart, 1);
        assert_eq!(lifecycle.last_policy_delay_ms, 250);
        assert_eq!(lifecycle.last_effective_restart_delay_ms, 500);
        assert_eq!(lifecycle.terminated, 1);
        assert_eq!(
            lifecycle.last_exit_kind,
            Some(GenerationExitKind::IsolatedStartupFailure)
        );
    }

    #[test]
    fn trigger_dispatch_recoverable_exit_updates_existing_lifecycle_counters() {
        let store = DiagnosticsStore::new();
        let service_id = ServiceId::new(22);
        let handle =
            store.register_generation(service_id, "email_trigger", 4, RuntimeLane::Standard);

        handle.record_exit(GenerationExitKind::RecoverableError);
        handle.record_restart(
            true,
            Duration::from_millis(10),
            Duration::from_millis(20),
            false,
        );

        let generation = handle.snapshot();
        assert_eq!(generation.aggregate.lifecycle.recoverable_error, 1);
        assert_eq!(generation.aggregate.lifecycle.backoff_restart, 1);
        assert_eq!(
            generation.aggregate.lifecycle.last_exit_kind,
            Some(GenerationExitKind::RecoverableError)
        );

        let service = store.service_snapshot(service_id).unwrap();
        assert_eq!(service.aggregate.lifecycle.recoverable_error, 1);
        assert_eq!(service.aggregate.lifecycle.backoff_restart, 1);
        assert_eq!(
            service.aggregate.lifecycle.last_exit_kind,
            Some(GenerationExitKind::RecoverableError)
        );
    }

    #[test]
    fn trigger_dispatch_panic_exit_updates_panic_counter_and_last_exit_kind() {
        let store = DiagnosticsStore::new();
        let service_id = ServiceId::new(23);
        let handle =
            store.register_generation(service_id, "panic_trigger", 5, RuntimeLane::Standard);

        handle.record_exit(GenerationExitKind::Panic);
        handle.record_restart(
            true,
            Duration::from_millis(10),
            Duration::from_millis(20),
            false,
        );

        let generation = handle.snapshot();
        assert_eq!(generation.aggregate.lifecycle.panic, 1);
        assert_eq!(generation.aggregate.lifecycle.backoff_restart, 1);
        assert_eq!(
            generation.aggregate.lifecycle.last_exit_kind,
            Some(GenerationExitKind::Panic)
        );

        let service = store.service_snapshot(service_id).unwrap();
        assert_eq!(service.aggregate.lifecycle.panic, 1);
        assert_eq!(service.aggregate.lifecycle.backoff_restart, 1);
        assert_eq!(
            service.aggregate.lifecycle.last_exit_kind,
            Some(GenerationExitKind::Panic)
        );
    }

    #[test]
    fn diagnostics_store_retains_only_recent_generation_snapshots_per_service() {
        let store = DiagnosticsStore::new();

        for generation in 1..=1030 {
            let handle = store.register_generation(
                ServiceId::new(5),
                "crashing",
                generation,
                RuntimeLane::Standard,
            );
            handle.record_sleep_observation(completed_observation(
                SleepObservationSource::ServiceSleep,
                Duration::from_millis(10),
                Duration::from_millis(11),
            ));
        }

        let snapshot = store.snapshot();
        let retained_generations: Vec<_> = snapshot
            .generations
            .iter()
            .filter(|generation| generation.service_id == ServiceId::new(5))
            .map(|generation| generation.generation)
            .collect();
        let service = store.service_snapshot(ServiceId::new(5)).unwrap();
        let lane = store.lane_snapshot(RuntimeLane::Standard);

        assert_eq!(retained_generations.len(), 1024);
        assert_eq!(retained_generations.first(), Some(&7));
        assert_eq!(retained_generations.last(), Some(&1030));
        assert_eq!(service.aggregate.service_sleep.completed, 1030);
        assert_eq!(lane.aggregate.service_sleep.completed, 1030);
    }

    #[test]
    fn diagnostics_generation_retention_is_service_scoped_and_keeps_aggregates() {
        let store = DiagnosticsStore::new();

        for generation in 1..=1030 {
            let handle = store.register_generation(
                ServiceId::new(5),
                "crashing",
                generation,
                RuntimeLane::Standard,
            );
            handle.record_sleep_observation(completed_observation(
                SleepObservationSource::ServiceSleep,
                Duration::from_millis(10),
                Duration::from_millis(12),
            ));
            handle.record_exit(GenerationExitKind::Panic);
        }

        for generation in 1..=3 {
            let handle = store.register_generation(
                ServiceId::new(6),
                "stable",
                generation,
                RuntimeLane::HighPriority,
            );
            handle.record_sleep_observation(completed_observation(
                SleepObservationSource::ServiceSleep,
                Duration::from_millis(10),
                Duration::from_millis(11),
            ));
            handle.record_exit(GenerationExitKind::NormalExit);
        }

        let snapshot = store.snapshot();
        let crashing_generations: Vec<_> = snapshot
            .generations
            .iter()
            .filter(|generation| generation.service_id == ServiceId::new(5))
            .map(|generation| generation.generation)
            .collect();
        let stable_generations: Vec<_> = snapshot
            .generations
            .iter()
            .filter(|generation| generation.service_id == ServiceId::new(6))
            .map(|generation| generation.generation)
            .collect();

        assert_eq!(crashing_generations.len(), 1024);
        assert_eq!(crashing_generations.first(), Some(&7));
        assert_eq!(crashing_generations.last(), Some(&1030));
        assert_eq!(stable_generations, vec![1, 2, 3]);
        assert!(store.generation_snapshot(ServiceId::new(5), 1).is_none());
        assert!(store.generation_snapshot(ServiceId::new(5), 7).is_some());
        assert!(store.generation_snapshot(ServiceId::new(6), 1).is_some());

        let crashing_service = store.service_snapshot(ServiceId::new(5)).unwrap();
        assert_eq!(crashing_service.current_generation, 1030);
        assert_eq!(crashing_service.aggregate.service_sleep.completed, 1030);
        assert_eq!(crashing_service.aggregate.lifecycle.panic, 1030);

        let stable_service = store.service_snapshot(ServiceId::new(6)).unwrap();
        assert_eq!(stable_service.current_generation, 3);
        assert_eq!(stable_service.aggregate.service_sleep.completed, 3);
        assert_eq!(stable_service.aggregate.lifecycle.normal_exit, 3);

        let standard_lane = store.lane_snapshot(RuntimeLane::Standard);
        assert_eq!(standard_lane.aggregate.service_sleep.completed, 1030);
        assert_eq!(standard_lane.aggregate.lifecycle.panic, 1030);

        let high_priority_lane = store.lane_snapshot(RuntimeLane::HighPriority);
        assert_eq!(high_priority_lane.aggregate.service_sleep.completed, 3);
        assert_eq!(high_priority_lane.aggregate.lifecycle.normal_exit, 3);
    }

    #[test]
    fn lane_observation_records_probe_without_service() {
        let store = DiagnosticsStore::new();

        store.record_lane_observation(
            RuntimeLane::Control,
            SleepObservation {
                source: SleepObservationSource::RuntimeProbe,
                reason: SleepExitReason::Completed,
                requested: Duration::from_millis(250),
                elapsed: Duration::from_millis(270),
                drift: Duration::from_millis(20),
            },
        );

        let lane = store.lane_snapshot(RuntimeLane::Control);
        assert_eq!(lane.aggregate.runtime_probe.completed, 1);
        assert_eq!(lane.aggregate.runtime_probe.total_drift_ms, 20);
        assert_eq!(store.snapshot().services.len(), 0);
    }

    #[test]
    fn public_snapshot_distills_internal_diagnostics() {
        let store = DiagnosticsStore::new();
        let handle =
            store.register_generation(ServiceId::new(4), "priority", 2, RuntimeLane::HighPriority);

        handle.record_sleep_observation(SleepObservation {
            source: SleepObservationSource::ServiceSleep,
            reason: SleepExitReason::Completed,
            requested: Duration::from_millis(25),
            elapsed: Duration::from_millis(40),
            drift: Duration::from_millis(15),
        });
        handle.record_exit(GenerationExitKind::RecoverableError);

        let snapshot: crate::models::DaemonDiagnosticsSnapshot = store.snapshot().into();
        assert_eq!(snapshot.services.len(), 1);
        assert_eq!(snapshot.generations.len(), 1);
        assert_eq!(snapshot.services[0].service_id, ServiceId::new(4));
        assert_eq!(
            snapshot.services[0].declared_scheduling,
            Some(ServiceScheduling::HighPriority)
        );
        assert_eq!(
            snapshot.generations[0].declared_scheduling,
            Some(ServiceScheduling::HighPriority)
        );
        assert_eq!(
            snapshot.generations[0]
                .aggregate
                .service_sleep
                .total_drift_ms,
            15
        );
        assert_eq!(
            snapshot.generations[0]
                .aggregate
                .lifecycle
                .recoverable_error,
            1
        );
        assert!(
            snapshot
                .lanes
                .iter()
                .any(|lane| { lane.runtime_lane == crate::models::DiagnosticRuntimeLane::Control })
        );
    }

    #[test]
    fn public_snapshot_labels_standard_lane_low_samples() {
        let store = DiagnosticsStore::new();

        let snapshot: crate::models::DaemonDiagnosticsSnapshot = store.snapshot().into();
        let standard = snapshot
            .lanes
            .iter()
            .find(|lane| lane.runtime_lane == crate::models::DiagnosticRuntimeLane::Standard)
            .expect("standard lane should be present");
        let labels = interpretation_labels(&standard.interpretations);

        assert!(
            labels.contains(&crate::models::DiagnosticInterpretationLabel::LowSampleSuppressed)
        );
    }

    #[test]
    fn public_snapshot_labels_standard_lane_wake_delay_and_wake_storm() {
        let store = DiagnosticsStore::new();
        record_standard_lane_probe(&store, Duration::from_millis(250));
        record_standard_lane_probe(&store, Duration::from_millis(250));
        record_standard_lane_probe(&store, Duration::from_millis(1800));

        let snapshot: crate::models::DaemonDiagnosticsSnapshot = store.snapshot().into();
        let standard = snapshot
            .lanes
            .iter()
            .find(|lane| lane.runtime_lane == crate::models::DiagnosticRuntimeLane::Standard)
            .expect("standard lane should be present");
        let labels = interpretation_labels(&standard.interpretations);

        assert!(labels.contains(
            &crate::models::DiagnosticInterpretationLabel::HostRuntimeWakeDelaySuspected
        ));
        assert!(labels.contains(&crate::models::DiagnosticInterpretationLabel::WakeStormSuspected));
    }

    #[test]
    fn public_snapshot_labels_standard_service_local_wake_delay() {
        let store = DiagnosticsStore::new();
        let handle =
            store.register_generation(ServiceId::new(10), "standard", 1, RuntimeLane::Standard);
        for _ in 0..3 {
            handle.record_sleep_observation(completed_observation(
                SleepObservationSource::ServiceSleep,
                Duration::from_millis(20),
                Duration::from_millis(320),
            ));
        }

        let snapshot: crate::models::DaemonDiagnosticsSnapshot = store.snapshot().into();
        let labels = interpretation_labels(&snapshot.services[0].interpretations);

        assert!(labels.contains(
            &crate::models::DiagnosticInterpretationLabel::ServiceLocalWakeDelaySuspected
        ));
        assert!(
            labels.contains(&crate::models::DiagnosticInterpretationLabel::BlockingRiskSuspected)
        );
    }

    #[test]
    fn public_snapshot_labels_standard_service_impacted_by_lane_pressure() {
        let store = DiagnosticsStore::new();
        let handle =
            store.register_generation(ServiceId::new(11), "standard", 1, RuntimeLane::Standard);
        for _ in 0..3 {
            record_standard_lane_probe(&store, Duration::from_millis(420));
            handle.record_sleep_observation(completed_observation(
                SleepObservationSource::ServiceSleep,
                Duration::from_millis(20),
                Duration::from_millis(180),
            ));
        }

        let snapshot: crate::models::DaemonDiagnosticsSnapshot = store.snapshot().into();
        let labels = interpretation_labels(&snapshot.services[0].interpretations);

        assert!(labels.contains(
            &crate::models::DiagnosticInterpretationLabel::ServiceImpactedByLanePressure
        ));
        assert!(!labels.contains(
            &crate::models::DiagnosticInterpretationLabel::ServiceLocalWakeDelaySuspected
        ));
    }

    #[test]
    fn public_snapshot_lifecycle_instability_takes_precedence_for_standard_service() {
        let store = DiagnosticsStore::new();
        let handle =
            store.register_generation(ServiceId::new(12), "standard", 1, RuntimeLane::Standard);
        for _ in 0..3 {
            handle.record_sleep_observation(completed_observation(
                SleepObservationSource::ServiceSleep,
                Duration::from_millis(20),
                Duration::from_millis(320),
            ));
        }
        handle.record_exit(GenerationExitKind::RecoverableError);
        handle.record_restart(false, Duration::ZERO, Duration::ZERO, false);
        handle.record_exit(GenerationExitKind::RecoverableError);
        handle.record_restart(false, Duration::ZERO, Duration::ZERO, false);

        let snapshot: crate::models::DaemonDiagnosticsSnapshot = store.snapshot().into();
        let labels = interpretation_labels(&snapshot.services[0].interpretations);

        assert_eq!(
            labels,
            vec![crate::models::DiagnosticInterpretationLabel::LifecycleInstability]
        );
    }

    #[test]
    fn public_snapshot_does_not_apply_standard_labels_to_high_priority_service() {
        let store = DiagnosticsStore::new();
        let handle =
            store.register_generation(ServiceId::new(13), "priority", 1, RuntimeLane::HighPriority);
        for _ in 0..3 {
            handle.record_sleep_observation(completed_observation(
                SleepObservationSource::ServiceSleep,
                Duration::from_millis(20),
                Duration::from_millis(320),
            ));
        }

        let snapshot: crate::models::DaemonDiagnosticsSnapshot = store.snapshot().into();

        assert!(snapshot.services[0].interpretations.is_empty());
    }

    #[test]
    fn public_snapshot_conversion_does_not_mutate_internal_diagnostics() {
        let store = DiagnosticsStore::new();
        let handle =
            store.register_generation(ServiceId::new(14), "standard", 1, RuntimeLane::Standard);
        for _ in 0..3 {
            record_standard_lane_probe(&store, Duration::from_millis(420));
            handle.record_sleep_observation(completed_observation(
                SleepObservationSource::ServiceSleep,
                Duration::from_millis(20),
                Duration::from_millis(180),
            ));
        }

        let before = store.snapshot();
        let public_snapshot: crate::models::DaemonDiagnosticsSnapshot = before.clone().into();

        assert!(!public_snapshot.services[0].interpretations.is_empty());
        assert_eq!(store.snapshot(), before);
    }

    #[tokio::test]
    async fn lane_runtime_probe_records_cancellation() {
        let store = Arc::new(DiagnosticsStore::new());
        let token = CancellationToken::new();
        token.cancel();

        run_lane_runtime_probe(store.clone(), RuntimeLane::Control, token).await;

        let lane = store.lane_snapshot(RuntimeLane::Control);
        assert_eq!(lane.aggregate.runtime_probe.completed, 0);
        assert_eq!(lane.aggregate.runtime_probe.interrupted, 1);
        assert_eq!(lane.aggregate.runtime_probe.total_drift_ms, 0);
    }

    #[tokio::test]
    async fn generation_runtime_probe_records_cancellation() {
        let store = DiagnosticsStore::new();
        let handle =
            store.register_generation(ServiceId::new(3), "isolated", 1, RuntimeLane::Isolated);
        let token = CancellationToken::new();
        token.cancel();

        run_generation_runtime_probe(handle.clone(), token).await;

        let snapshot = handle.snapshot();
        assert_eq!(snapshot.aggregate.runtime_probe.completed, 0);
        assert_eq!(snapshot.aggregate.runtime_probe.interrupted, 1);
        assert_eq!(snapshot.aggregate.runtime_probe.total_drift_ms, 0);
    }
}
