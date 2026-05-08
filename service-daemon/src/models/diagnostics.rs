use crate::core::diagnostics as internal;

use super::service::{ServiceId, ServiceScheduling};

const DIAGNOSTIC_MINIMUM_COMPLETED_SAMPLES: u64 = 3;
const DIAGNOSTIC_HIGH_AVG_DRIFT_MS: u64 = 100;
const DIAGNOSTIC_BLOCKING_RISK_AVG_DRIFT_MS: u64 = 250;
const DIAGNOSTIC_WAKE_STORM_MAX_DRIFT_MS: u64 = 500;
const DIAGNOSTIC_RESTART_INSTABILITY_THRESHOLD: u64 = 2;

/// Observation-only runtime lane in diagnostics snapshots.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DiagnosticRuntimeLane {
    /// Daemon-owned control-plane lane for supervision and diagnostics work.
    Control,
    /// Host runtime lane for `Standard` service and trigger bodies.
    Standard,
    /// Daemon-owned high-priority runtime lane.
    HighPriority,
    /// Per-generation private thread and runtime lane.
    Isolated,
}

impl From<internal::RuntimeLane> for DiagnosticRuntimeLane {
    fn from(value: internal::RuntimeLane) -> Self {
        match value {
            internal::RuntimeLane::Control => Self::Control,
            internal::RuntimeLane::Standard => Self::Standard,
            internal::RuntimeLane::HighPriority => Self::HighPriority,
            internal::RuntimeLane::Isolated => Self::Isolated,
        }
    }
}

/// Confidence level for best-effort diagnostic interpretation labels.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DiagnosticConfidence {
    /// The signal is weak or sample quality is low.
    Low,
    /// The signal is sustained enough to investigate.
    Medium,
    /// The signal is strong, but still not a command or definitive root cause.
    High,
}

/// Best-effort interpretation label derived from read-only diagnostics facts.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DiagnosticInterpretationLabel {
    /// There are not enough completed samples for a stronger interpretation.
    LowSampleSuppressed,
    /// Standard host-runtime wakeups appear delayed.
    HostRuntimeWakeDelaySuspected,
    /// A Standard service's own wakeups appear delayed.
    ServiceLocalWakeDelaySuspected,
    /// A Standard service may be impacted by broader Standard lane pressure.
    ServiceImpactedByLanePressure,
    /// Blocking or CPU-bound work may be delaying cooperative async wakeups.
    BlockingRiskSuspected,
    /// A small number of large wakeup delays suggests bursty wake pressure.
    WakeStormSuspected,
    /// Lifecycle instability is a stronger signal than placement-like advice.
    LifecycleInstability,
}

/// Human-facing investigation hint attached to a diagnostic interpretation.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DiagnosticRecommendationHint {
    /// Keep collecting samples before acting.
    ContinueObserving,
    /// Inspect the Tokio runtime that owns `Standard` service bodies.
    InvestigateHostRuntime,
    /// Audit blocking syscalls, CPU-bound loops, or missing `spawn_blocking` boundaries.
    CheckBlockingWork,
    /// Add business-level spans or metrics to identify culprit versus victim.
    AddBusinessTracing,
    /// Consider changing the source-level declared mode in a future build.
    ConsiderDeclaredModeChange,
    /// Investigate restart/backoff/failure causes before placement questions.
    InvestigateLifecycleInstability,
}

/// Read-only interpretation derived from diagnostics counters.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticInterpretation {
    /// Best-effort label inferred from observed counters.
    pub label: DiagnosticInterpretationLabel,
    /// Confidence in the label based on sample count and signal strength.
    pub confidence: DiagnosticConfidence,
    /// Non-command investigation hints for humans.
    pub recommendations: Vec<DiagnosticRecommendationHint>,
}

/// Last recorded generation exit classification.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DiagnosticGenerationExitKind {
    /// The generation returned successfully.
    NormalExit,
    /// The generation returned a recoverable error.
    RecoverableError,
    /// The generation panicked.
    Panic,
    /// The generation returned a fatal service error.
    FatalServiceError,
    /// Provider initialization failed at the service boundary.
    ProviderInitError,
    /// The generation exited for reload.
    Reload,
    /// The generation exited for shutdown.
    Shutdown,
    /// Isolated thread/runtime startup failed before the body could run.
    IsolatedStartupFailure,
}

impl From<internal::GenerationExitKind> for DiagnosticGenerationExitKind {
    fn from(value: internal::GenerationExitKind) -> Self {
        match value {
            internal::GenerationExitKind::NormalExit => Self::NormalExit,
            internal::GenerationExitKind::RecoverableError => Self::RecoverableError,
            internal::GenerationExitKind::Panic => Self::Panic,
            internal::GenerationExitKind::FatalServiceError => Self::FatalServiceError,
            internal::GenerationExitKind::ProviderInitError => Self::ProviderInitError,
            internal::GenerationExitKind::Reload => Self::Reload,
            internal::GenerationExitKind::Shutdown => Self::Shutdown,
            internal::GenerationExitKind::IsolatedStartupFailure => Self::IsolatedStartupFailure,
        }
    }
}

/// Aggregated sleep/probe observation counters.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiagnosticObservationStats {
    /// Completed observations that contributed drift samples.
    pub completed: u64,
    /// Interrupted observations such as reload, shutdown, or cancelled probes.
    pub interrupted: u64,
    /// Total requested duration in milliseconds.
    pub total_requested_ms: u64,
    /// Total elapsed duration in milliseconds.
    pub total_elapsed_ms: u64,
    /// Total completed-observation drift in milliseconds.
    pub total_drift_ms: u64,
    /// Maximum completed-observation drift in milliseconds.
    pub max_drift_ms: u64,
    /// Last completed-observation drift in milliseconds.
    pub last_drift_ms: u64,
    /// Average completed-observation drift in milliseconds.
    pub avg_drift_ms: u64,
}

impl From<internal::ObservationStatsSnapshot> for DiagnosticObservationStats {
    fn from(value: internal::ObservationStatsSnapshot) -> Self {
        Self {
            completed: value.completed,
            interrupted: value.interrupted,
            total_requested_ms: value.total_requested_ms,
            total_elapsed_ms: value.total_elapsed_ms,
            total_drift_ms: value.total_drift_ms,
            max_drift_ms: value.max_drift_ms,
            last_drift_ms: value.last_drift_ms,
            avg_drift_ms: value.avg_drift_ms,
        }
    }
}

/// Aggregated lifecycle counters for a service, generation, or diagnostics lane.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiagnosticLifecycleStats {
    /// Reload requests observed for the record.
    pub reload_requested: u64,
    /// Generations that exited for reload.
    pub reload_exit: u64,
    /// Restart decisions recorded for recoverable exits.
    pub restart: u64,
    /// Restart decisions that used backoff.
    pub backoff_restart: u64,
    /// Restart decisions extended by rate limiting.
    pub rate_limited_restart: u64,
    /// Termination observations recorded by the supervisor.
    pub terminated: u64,
    /// Successful generation exits.
    pub normal_exit: u64,
    /// Recoverable error exits.
    pub recoverable_error: u64,
    /// Panic exits.
    pub panic: u64,
    /// Fatal service error exits.
    pub fatal_service_error: u64,
    /// Provider initialization error exits.
    pub provider_init_error: u64,
    /// Shutdown exits.
    pub shutdown: u64,
    /// Isolated startup failures.
    pub isolated_startup_failure: u64,
    /// Last configured policy delay in milliseconds.
    pub last_policy_delay_ms: u64,
    /// Last effective restart delay in milliseconds.
    pub last_effective_restart_delay_ms: u64,
    /// Last recorded exit classification.
    pub last_exit_kind: Option<DiagnosticGenerationExitKind>,
}

impl From<internal::LifecycleStatsSnapshot> for DiagnosticLifecycleStats {
    fn from(value: internal::LifecycleStatsSnapshot) -> Self {
        Self {
            reload_requested: value.reload_requested,
            reload_exit: value.reload_exit,
            restart: value.restart,
            backoff_restart: value.backoff_restart,
            rate_limited_restart: value.rate_limited_restart,
            terminated: value.terminated,
            normal_exit: value.normal_exit,
            recoverable_error: value.recoverable_error,
            panic: value.panic,
            fatal_service_error: value.fatal_service_error,
            provider_init_error: value.provider_init_error,
            shutdown: value.shutdown,
            isolated_startup_failure: value.isolated_startup_failure,
            last_policy_delay_ms: value.last_policy_delay_ms,
            last_effective_restart_delay_ms: value.last_effective_restart_delay_ms,
            last_exit_kind: value.last_exit_kind.map(Into::into),
        }
    }
}

/// Observation and lifecycle aggregates for one diagnostics record.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiagnosticAggregateStats {
    /// `service_daemon::sleep()` observations from service or trigger bodies.
    pub service_sleep: DiagnosticObservationStats,
    /// Runtime heartbeat probe observations.
    pub runtime_probe: DiagnosticObservationStats,
    /// Lifecycle outcome counters.
    pub lifecycle: DiagnosticLifecycleStats,
}

impl From<internal::DiagnosticsAggregateSnapshot> for DiagnosticAggregateStats {
    fn from(value: internal::DiagnosticsAggregateSnapshot) -> Self {
        Self {
            service_sleep: value.service_sleep.into(),
            runtime_probe: value.runtime_probe.into(),
            lifecycle: value.lifecycle.into(),
        }
    }
}

/// Read-only diagnostics for a service across generations.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceDiagnosticsSnapshot {
    /// Static service identifier.
    pub service_id: ServiceId,
    /// Registered service name.
    pub service_name: &'static str,
    /// Latest generation number observed for this service.
    pub current_generation: u64,
    /// Static scheduling declaration for the service body.
    pub declared_scheduling: Option<ServiceScheduling>,
    /// Aggregated diagnostics for the service.
    pub aggregate: DiagnosticAggregateStats,
    /// Best-effort read-only diagnostic interpretations for this service.
    pub interpretations: Vec<DiagnosticInterpretation>,
}

impl ServiceDiagnosticsSnapshot {
    fn from_internal(
        value: internal::ServiceDiagnosticsSnapshot,
        standard_lane_pressure: bool,
    ) -> Self {
        let interpretations = service_interpretations(&value, standard_lane_pressure);
        Self {
            service_id: value.service_id,
            service_name: value.service_name,
            current_generation: value.current_generation,
            declared_scheduling: scheduling_from_lane(value.runtime_lane),
            aggregate: value.aggregate.into(),
            interpretations,
        }
    }
}

impl From<internal::ServiceDiagnosticsSnapshot> for ServiceDiagnosticsSnapshot {
    fn from(value: internal::ServiceDiagnosticsSnapshot) -> Self {
        Self::from_internal(value, false)
    }
}

/// Read-only diagnostics for a single service generation.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenerationDiagnosticsSnapshot {
    /// Static service identifier.
    pub service_id: ServiceId,
    /// Registered service name.
    pub service_name: &'static str,
    /// Generation number.
    pub generation: u64,
    /// Static scheduling declaration for the generation body.
    pub declared_scheduling: Option<ServiceScheduling>,
    /// Aggregated diagnostics for the generation.
    pub aggregate: DiagnosticAggregateStats,
}

impl From<internal::GenerationDiagnosticsSnapshot> for GenerationDiagnosticsSnapshot {
    fn from(value: internal::GenerationDiagnosticsSnapshot) -> Self {
        Self {
            service_id: value.service_id,
            service_name: value.service_name,
            generation: value.generation,
            declared_scheduling: scheduling_from_lane(value.runtime_lane),
            aggregate: value.aggregate.into(),
        }
    }
}

/// Read-only diagnostics for a logical runtime lane.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeLaneDiagnosticsSnapshot {
    /// Observation-only diagnostics lane.
    pub runtime_lane: DiagnosticRuntimeLane,
    /// Aggregated diagnostics for the lane.
    pub aggregate: DiagnosticAggregateStats,
    /// Best-effort read-only diagnostic interpretations for this lane.
    pub interpretations: Vec<DiagnosticInterpretation>,
}

impl From<internal::RuntimeLaneSnapshot> for RuntimeLaneDiagnosticsSnapshot {
    fn from(value: internal::RuntimeLaneSnapshot) -> Self {
        let interpretations = lane_interpretations(&value);
        Self {
            runtime_lane: value.runtime_lane.into(),
            aggregate: value.aggregate.into(),
            interpretations,
        }
    }
}

/// Read-only diagnostics snapshot for the daemon.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonDiagnosticsSnapshot {
    /// Service-level summaries.
    pub services: Vec<ServiceDiagnosticsSnapshot>,
    /// Generation-level summaries.
    pub generations: Vec<GenerationDiagnosticsSnapshot>,
    /// Logical runtime lane summaries, including internal `Control` diagnostics.
    pub lanes: Vec<RuntimeLaneDiagnosticsSnapshot>,
}

impl From<internal::DiagnosticsSnapshot> for DaemonDiagnosticsSnapshot {
    fn from(value: internal::DiagnosticsSnapshot) -> Self {
        let standard_lane_pressure = value.lanes.iter().any(standard_lane_has_pressure);
        Self {
            services: value
                .services
                .into_iter()
                .map(|service| {
                    ServiceDiagnosticsSnapshot::from_internal(service, standard_lane_pressure)
                })
                .collect(),
            generations: value.generations.into_iter().map(Into::into).collect(),
            lanes: value.lanes.into_iter().map(Into::into).collect(),
        }
    }
}

fn lane_interpretations(lane: &internal::RuntimeLaneSnapshot) -> Vec<DiagnosticInterpretation> {
    if lane.runtime_lane != internal::RuntimeLane::Standard {
        return Vec::new();
    }

    let observation = &lane.aggregate.runtime_probe;
    if observation_is_low_sample(observation) {
        return vec![interpretation(
            DiagnosticInterpretationLabel::LowSampleSuppressed,
            DiagnosticConfidence::Low,
            &[DiagnosticRecommendationHint::ContinueObserving],
        )];
    }

    let mut interpretations = Vec::new();
    if observation_has_drift_pressure(observation) {
        interpretations.push(interpretation(
            DiagnosticInterpretationLabel::HostRuntimeWakeDelaySuspected,
            confidence_for_observation(observation),
            &[
                DiagnosticRecommendationHint::InvestigateHostRuntime,
                DiagnosticRecommendationHint::CheckBlockingWork,
            ],
        ));
    }
    if observation_has_wake_storm(observation) {
        interpretations.push(interpretation(
            DiagnosticInterpretationLabel::WakeStormSuspected,
            DiagnosticConfidence::Medium,
            &[
                DiagnosticRecommendationHint::InvestigateHostRuntime,
                DiagnosticRecommendationHint::AddBusinessTracing,
            ],
        ));
    }

    interpretations
}

fn service_interpretations(
    service: &internal::ServiceDiagnosticsSnapshot,
    standard_lane_pressure: bool,
) -> Vec<DiagnosticInterpretation> {
    if service.runtime_lane != internal::RuntimeLane::Standard {
        return Vec::new();
    }

    let lifecycle = &service.aggregate.lifecycle;
    if lifecycle_has_instability(lifecycle) {
        return vec![interpretation(
            DiagnosticInterpretationLabel::LifecycleInstability,
            DiagnosticConfidence::High,
            &[DiagnosticRecommendationHint::InvestigateLifecycleInstability],
        )];
    }

    let observation = &service.aggregate.service_sleep;
    if observation_is_low_sample(observation) {
        return vec![interpretation(
            DiagnosticInterpretationLabel::LowSampleSuppressed,
            DiagnosticConfidence::Low,
            &[DiagnosticRecommendationHint::ContinueObserving],
        )];
    }

    let mut interpretations = Vec::new();
    if observation_has_drift_pressure(observation) {
        if standard_lane_pressure {
            interpretations.push(interpretation(
                DiagnosticInterpretationLabel::ServiceImpactedByLanePressure,
                DiagnosticConfidence::Low,
                &[
                    DiagnosticRecommendationHint::InvestigateHostRuntime,
                    DiagnosticRecommendationHint::AddBusinessTracing,
                ],
            ));
        } else {
            interpretations.push(interpretation(
                DiagnosticInterpretationLabel::ServiceLocalWakeDelaySuspected,
                DiagnosticConfidence::Low,
                &[
                    DiagnosticRecommendationHint::CheckBlockingWork,
                    DiagnosticRecommendationHint::AddBusinessTracing,
                    DiagnosticRecommendationHint::ConsiderDeclaredModeChange,
                ],
            ));
        }
    }
    if observation.avg_drift_ms >= DIAGNOSTIC_BLOCKING_RISK_AVG_DRIFT_MS {
        interpretations.push(interpretation(
            DiagnosticInterpretationLabel::BlockingRiskSuspected,
            confidence_for_observation(observation),
            &[
                DiagnosticRecommendationHint::CheckBlockingWork,
                DiagnosticRecommendationHint::AddBusinessTracing,
            ],
        ));
    }
    if observation_has_wake_storm(observation) {
        interpretations.push(interpretation(
            DiagnosticInterpretationLabel::WakeStormSuspected,
            DiagnosticConfidence::Medium,
            &[
                DiagnosticRecommendationHint::AddBusinessTracing,
                DiagnosticRecommendationHint::InvestigateHostRuntime,
            ],
        ));
    }

    interpretations
}

fn standard_lane_has_pressure(lane: &internal::RuntimeLaneSnapshot) -> bool {
    lane.runtime_lane == internal::RuntimeLane::Standard
        && observation_has_drift_pressure(&lane.aggregate.runtime_probe)
}

fn observation_is_low_sample(observation: &internal::ObservationStatsSnapshot) -> bool {
    observation.completed < DIAGNOSTIC_MINIMUM_COMPLETED_SAMPLES
}

fn observation_has_drift_pressure(observation: &internal::ObservationStatsSnapshot) -> bool {
    !observation_is_low_sample(observation)
        && observation.avg_drift_ms >= DIAGNOSTIC_HIGH_AVG_DRIFT_MS
}

fn observation_has_wake_storm(observation: &internal::ObservationStatsSnapshot) -> bool {
    !observation_is_low_sample(observation)
        && observation.max_drift_ms >= DIAGNOSTIC_WAKE_STORM_MAX_DRIFT_MS
        && observation.max_drift_ms >= observation.avg_drift_ms.saturating_mul(3)
}

fn lifecycle_has_instability(lifecycle: &internal::LifecycleStatsSnapshot) -> bool {
    lifecycle.rate_limited_restart > 0
        || lifecycle.backoff_restart >= DIAGNOSTIC_RESTART_INSTABILITY_THRESHOLD
        || lifecycle.restart >= DIAGNOSTIC_RESTART_INSTABILITY_THRESHOLD
        || lifecycle.recoverable_error >= DIAGNOSTIC_RESTART_INSTABILITY_THRESHOLD
        || lifecycle.panic > 0
        || lifecycle.fatal_service_error > 0
        || lifecycle.provider_init_error > 0
}

fn confidence_for_observation(
    observation: &internal::ObservationStatsSnapshot,
) -> DiagnosticConfidence {
    if observation.completed >= DIAGNOSTIC_MINIMUM_COMPLETED_SAMPLES.saturating_mul(2)
        && observation.avg_drift_ms >= DIAGNOSTIC_HIGH_AVG_DRIFT_MS.saturating_mul(2)
    {
        DiagnosticConfidence::High
    } else {
        DiagnosticConfidence::Medium
    }
}

fn interpretation(
    label: DiagnosticInterpretationLabel,
    confidence: DiagnosticConfidence,
    recommendations: &[DiagnosticRecommendationHint],
) -> DiagnosticInterpretation {
    DiagnosticInterpretation {
        label,
        confidence,
        recommendations: recommendations.to_vec(),
    }
}

fn scheduling_from_lane(lane: internal::RuntimeLane) -> Option<ServiceScheduling> {
    match lane {
        internal::RuntimeLane::Control => None,
        internal::RuntimeLane::Standard => Some(ServiceScheduling::Standard),
        internal::RuntimeLane::HighPriority => Some(ServiceScheduling::HighPriority),
        internal::RuntimeLane::Isolated => Some(ServiceScheduling::Isolated),
    }
}
