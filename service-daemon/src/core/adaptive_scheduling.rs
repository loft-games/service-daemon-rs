use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;

use crate::core::diagnostics::{
    DiagnosticsAggregateSnapshot, DiagnosticsSnapshot, DiagnosticsStore,
    GenerationDiagnosticsSnapshot, LifecycleStatsSnapshot, ObservationStatsSnapshot, RuntimeLane,
    RuntimeLaneSnapshot, ServiceDiagnosticsSnapshot,
};
use crate::models::ServiceId;

const RECOMMENDATION_INTERVAL: Duration = Duration::from_secs(30);
const MINIMUM_COMPLETED_SAMPLES: u64 = 3;
const HIGH_AVG_DRIFT_MS: u64 = 100;
const RESTART_INSTABILITY_THRESHOLD: u64 = 2;
const ISOLATED_PRESSURE_THRESHOLD: u64 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ObservationWindow {
    pub completed: u64,
    pub interrupted: u64,
    pub total_requested_ms: u64,
    pub total_elapsed_ms: u64,
    pub total_drift_ms: u64,
    pub proven_max_drift_ms: Option<u64>,
    pub last_drift_ms: u64,
    pub avg_drift_ms: u64,
}

impl ObservationWindow {
    pub(crate) fn is_low_sample(&self, minimum_completed_samples: u64) -> bool {
        self.completed < minimum_completed_samples
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LifecycleWindow {
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DiagnosticsAggregateWindow {
    pub service_sleep: ObservationWindow,
    pub runtime_probe: ObservationWindow,
    pub lifecycle: LifecycleWindow,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ServiceDiagnosticsWindow {
    pub service_id: ServiceId,
    pub service_name: &'static str,
    pub current_generation: u64,
    pub runtime_lane: RuntimeLane,
    pub aggregate: DiagnosticsAggregateWindow,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GenerationDiagnosticsWindow {
    pub service_id: ServiceId,
    pub service_name: &'static str,
    pub generation: u64,
    pub runtime_lane: RuntimeLane,
    pub aggregate: DiagnosticsAggregateWindow,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimeLaneWindow {
    pub runtime_lane: RuntimeLane,
    pub aggregate: DiagnosticsAggregateWindow,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DiagnosticsWindow {
    pub has_baseline: bool,
    pub services: Vec<ServiceDiagnosticsWindow>,
    pub generations: Vec<GenerationDiagnosticsWindow>,
    pub lanes: Vec<RuntimeLaneWindow>,
}

#[derive(Default)]
pub(crate) struct DiagnosticsSampler {
    previous: Option<DiagnosticsSnapshot>,
}

impl DiagnosticsSampler {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn sample(&mut self, current: DiagnosticsSnapshot) -> DiagnosticsWindow {
        let window = DiagnosticsWindow::from_snapshots(self.previous.as_ref(), &current);
        self.previous = Some(current);
        window
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SchedulingRecommendation {
    pub target: SchedulingRecommendationTarget,
    pub current_lane: Option<RuntimeLane>,
    pub action: SchedulingRecommendationAction,
    pub reason: SchedulingRecommendationReason,
    pub confidence: RecommendationConfidence,
    pub observation: RecommendationObservation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SchedulingRecommendationTarget {
    RuntimeLane(RuntimeLane),
    Service {
        service_id: ServiceId,
        service_name: &'static str,
        current_generation: u64,
    },
    Generation {
        service_id: ServiceId,
        service_name: &'static str,
        generation: u64,
        runtime_lane: RuntimeLane,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SchedulingRecommendationAction {
    KeepCurrentLane,
    Observe,
    InvestigateControlPlane,
    ConsiderIsolation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SchedulingRecommendationReason {
    StandardLanePressure,
    StandardServiceSleepDrift,
    ControlPlanePressure,
    HighPrioritySaturation,
    IsolatedResourcePressure,
    LifecycleInstability,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RecommendationConfidence {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RecommendationObservation {
    pub completed_samples: u64,
    pub interrupted_samples: u64,
    pub total_requested_ms: u64,
    pub total_elapsed_ms: u64,
    pub total_drift_ms: u64,
    pub proven_max_drift_ms: Option<u64>,
    pub last_drift_ms: u64,
    pub avg_drift_ms: u64,
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
}

impl RecommendationObservation {
    fn from_aggregate(
        aggregate: &DiagnosticsAggregateWindow,
        observation: &ObservationWindow,
    ) -> Self {
        let lifecycle = &aggregate.lifecycle;
        Self {
            completed_samples: observation.completed,
            interrupted_samples: observation.interrupted,
            total_requested_ms: observation.total_requested_ms,
            total_elapsed_ms: observation.total_elapsed_ms,
            total_drift_ms: observation.total_drift_ms,
            proven_max_drift_ms: observation.proven_max_drift_ms,
            last_drift_ms: observation.last_drift_ms,
            avg_drift_ms: observation.avg_drift_ms,
            reload_requested: lifecycle.reload_requested,
            reload_exit: lifecycle.reload_exit,
            restart: lifecycle.restart,
            backoff_restart: lifecycle.backoff_restart,
            rate_limited_restart: lifecycle.rate_limited_restart,
            terminated: lifecycle.terminated,
            normal_exit: lifecycle.normal_exit,
            recoverable_error: lifecycle.recoverable_error,
            panic: lifecycle.panic,
            fatal_service_error: lifecycle.fatal_service_error,
            provider_init_error: lifecycle.provider_init_error,
            shutdown: lifecycle.shutdown,
            isolated_startup_failure: lifecycle.isolated_startup_failure,
            last_policy_delay_ms: lifecycle.last_policy_delay_ms,
            last_effective_restart_delay_ms: lifecycle.last_effective_restart_delay_ms,
        }
    }
}

#[derive(Default)]
pub(crate) struct SchedulingPolicyEvaluator {
    thresholds: SchedulingPolicyThresholds,
}

impl SchedulingPolicyEvaluator {
    pub(crate) fn evaluate(&self, window: &DiagnosticsWindow) -> Vec<SchedulingRecommendation> {
        if !window.has_baseline {
            return Vec::new();
        }

        let standard_lane_pressure =
            self.runtime_lane_has_probe_pressure(window, RuntimeLane::Standard);
        let isolated_pressure = self.has_isolated_pressure(window);
        let mut recommendations = Vec::new();

        for lane in &window.lanes {
            self.evaluate_lane(lane, &mut recommendations);
        }

        for generation in &window.generations {
            self.evaluate_generation(generation, &mut recommendations);
        }

        for service in &window.services {
            self.evaluate_service(
                service,
                standard_lane_pressure,
                isolated_pressure,
                &mut recommendations,
            );
        }

        recommendations
    }

    fn evaluate_lane(
        &self,
        lane: &RuntimeLaneWindow,
        recommendations: &mut Vec<SchedulingRecommendation>,
    ) {
        if self.observation_has_drift_pressure(&lane.aggregate.runtime_probe) {
            let (action, reason) = match lane.runtime_lane {
                RuntimeLane::Control => (
                    SchedulingRecommendationAction::InvestigateControlPlane,
                    SchedulingRecommendationReason::ControlPlanePressure,
                ),
                RuntimeLane::Standard => (
                    SchedulingRecommendationAction::Observe,
                    SchedulingRecommendationReason::StandardLanePressure,
                ),
                RuntimeLane::HighPriority => (
                    SchedulingRecommendationAction::Observe,
                    SchedulingRecommendationReason::HighPrioritySaturation,
                ),
                RuntimeLane::Isolated => (
                    SchedulingRecommendationAction::Observe,
                    SchedulingRecommendationReason::IsolatedResourcePressure,
                ),
            };
            recommendations.push(SchedulingRecommendation {
                target: SchedulingRecommendationTarget::RuntimeLane(lane.runtime_lane),
                current_lane: Some(lane.runtime_lane),
                action,
                reason,
                confidence: self.confidence_for_observation(&lane.aggregate.runtime_probe),
                observation: RecommendationObservation::from_aggregate(
                    &lane.aggregate,
                    &lane.aggregate.runtime_probe,
                ),
            });
        }

        if lane.runtime_lane == RuntimeLane::Isolated
            && self.lifecycle_has_isolated_pressure(&lane.aggregate.lifecycle)
        {
            recommendations.push(SchedulingRecommendation {
                target: SchedulingRecommendationTarget::RuntimeLane(RuntimeLane::Isolated),
                current_lane: Some(RuntimeLane::Isolated),
                action: SchedulingRecommendationAction::Observe,
                reason: SchedulingRecommendationReason::IsolatedResourcePressure,
                confidence: RecommendationConfidence::High,
                observation: RecommendationObservation::from_aggregate(
                    &lane.aggregate,
                    &lane.aggregate.runtime_probe,
                ),
            });
        }
    }

    fn evaluate_generation(
        &self,
        generation: &GenerationDiagnosticsWindow,
        recommendations: &mut Vec<SchedulingRecommendation>,
    ) {
        if self.lifecycle_has_instability(&generation.aggregate.lifecycle) {
            recommendations.push(SchedulingRecommendation {
                target: SchedulingRecommendationTarget::Generation {
                    service_id: generation.service_id,
                    service_name: generation.service_name,
                    generation: generation.generation,
                    runtime_lane: generation.runtime_lane,
                },
                current_lane: Some(generation.runtime_lane),
                action: SchedulingRecommendationAction::KeepCurrentLane,
                reason: SchedulingRecommendationReason::LifecycleInstability,
                confidence: RecommendationConfidence::High,
                observation: RecommendationObservation::from_aggregate(
                    &generation.aggregate,
                    &generation.aggregate.service_sleep,
                ),
            });
            return;
        }

        if generation.runtime_lane == RuntimeLane::Isolated
            && self.lifecycle_has_isolated_pressure(&generation.aggregate.lifecycle)
        {
            recommendations.push(SchedulingRecommendation {
                target: SchedulingRecommendationTarget::Generation {
                    service_id: generation.service_id,
                    service_name: generation.service_name,
                    generation: generation.generation,
                    runtime_lane: generation.runtime_lane,
                },
                current_lane: Some(generation.runtime_lane),
                action: SchedulingRecommendationAction::Observe,
                reason: SchedulingRecommendationReason::IsolatedResourcePressure,
                confidence: RecommendationConfidence::High,
                observation: RecommendationObservation::from_aggregate(
                    &generation.aggregate,
                    &generation.aggregate.service_sleep,
                ),
            });
        }
    }

    fn evaluate_service(
        &self,
        service: &ServiceDiagnosticsWindow,
        standard_lane_pressure: bool,
        isolated_pressure: bool,
        recommendations: &mut Vec<SchedulingRecommendation>,
    ) {
        if self.lifecycle_has_instability(&service.aggregate.lifecycle) {
            recommendations.push(SchedulingRecommendation {
                target: SchedulingRecommendationTarget::Service {
                    service_id: service.service_id,
                    service_name: service.service_name,
                    current_generation: service.current_generation,
                },
                current_lane: Some(service.runtime_lane),
                action: SchedulingRecommendationAction::KeepCurrentLane,
                reason: SchedulingRecommendationReason::LifecycleInstability,
                confidence: RecommendationConfidence::High,
                observation: RecommendationObservation::from_aggregate(
                    &service.aggregate,
                    &service.aggregate.service_sleep,
                ),
            });
            return;
        }

        if service.runtime_lane != RuntimeLane::Standard
            || !self.observation_has_drift_pressure(&service.aggregate.service_sleep)
        {
            return;
        }

        let action = if standard_lane_pressure || isolated_pressure {
            SchedulingRecommendationAction::Observe
        } else {
            SchedulingRecommendationAction::ConsiderIsolation
        };

        recommendations.push(SchedulingRecommendation {
            target: SchedulingRecommendationTarget::Service {
                service_id: service.service_id,
                service_name: service.service_name,
                current_generation: service.current_generation,
            },
            current_lane: Some(service.runtime_lane),
            action,
            reason: SchedulingRecommendationReason::StandardServiceSleepDrift,
            confidence: RecommendationConfidence::Low,
            observation: RecommendationObservation::from_aggregate(
                &service.aggregate,
                &service.aggregate.service_sleep,
            ),
        });
    }

    fn runtime_lane_has_probe_pressure(
        &self,
        window: &DiagnosticsWindow,
        lane: RuntimeLane,
    ) -> bool {
        window.lanes.iter().any(|window| {
            window.runtime_lane == lane
                && self.observation_has_drift_pressure(&window.aggregate.runtime_probe)
        })
    }

    fn has_isolated_pressure(&self, window: &DiagnosticsWindow) -> bool {
        window.lanes.iter().any(|lane| {
            lane.runtime_lane == RuntimeLane::Isolated
                && self.lifecycle_has_isolated_pressure(&lane.aggregate.lifecycle)
        }) || window.generations.iter().any(|generation| {
            generation.runtime_lane == RuntimeLane::Isolated
                && self.lifecycle_has_isolated_pressure(&generation.aggregate.lifecycle)
        })
    }

    fn observation_has_drift_pressure(&self, observation: &ObservationWindow) -> bool {
        !observation.is_low_sample(self.thresholds.minimum_completed_samples)
            && observation.avg_drift_ms >= self.thresholds.high_avg_drift_ms
    }

    fn lifecycle_has_instability(&self, lifecycle: &LifecycleWindow) -> bool {
        lifecycle.rate_limited_restart > 0
            || lifecycle.backoff_restart >= self.thresholds.restart_instability_threshold
            || lifecycle.restart >= self.thresholds.restart_instability_threshold
            || lifecycle.recoverable_error >= self.thresholds.restart_instability_threshold
            || lifecycle.panic > 0
            || lifecycle.fatal_service_error > 0
            || lifecycle.provider_init_error > 0
    }

    fn lifecycle_has_isolated_pressure(&self, lifecycle: &LifecycleWindow) -> bool {
        lifecycle.isolated_startup_failure + lifecycle.rate_limited_restart
            >= self.thresholds.isolated_pressure_threshold
    }

    fn confidence_for_observation(
        &self,
        observation: &ObservationWindow,
    ) -> RecommendationConfidence {
        if observation.completed >= self.thresholds.minimum_completed_samples.saturating_mul(2)
            && observation.avg_drift_ms >= self.thresholds.high_avg_drift_ms.saturating_mul(2)
        {
            RecommendationConfidence::High
        } else {
            RecommendationConfidence::Medium
        }
    }
}

struct SchedulingPolicyThresholds {
    minimum_completed_samples: u64,
    high_avg_drift_ms: u64,
    restart_instability_threshold: u64,
    isolated_pressure_threshold: u64,
}

impl Default for SchedulingPolicyThresholds {
    fn default() -> Self {
        Self {
            minimum_completed_samples: MINIMUM_COMPLETED_SAMPLES,
            high_avg_drift_ms: HIGH_AVG_DRIFT_MS,
            restart_instability_threshold: RESTART_INSTABILITY_THRESHOLD,
            isolated_pressure_threshold: ISOLATED_PRESSURE_THRESHOLD,
        }
    }
}

pub(crate) async fn run_adaptive_scheduling_recommendations(
    diagnostics: Arc<DiagnosticsStore>,
    token: CancellationToken,
) {
    let mut sampler = DiagnosticsSampler::new();
    let evaluator = SchedulingPolicyEvaluator::default();
    let mut interval = tokio::time::interval(RECOMMENDATION_INTERVAL);
    interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut previous_fingerprint = Vec::new();

    loop {
        tokio::select! {
            _ = token.cancelled() => break,
            _ = interval.tick() => {
                let window = sampler.sample(diagnostics.snapshot());
                if !window.has_baseline {
                    continue;
                }

                let recommendations = evaluator.evaluate(&window);
                let fingerprint = recommendation_fingerprint(&recommendations);
                if fingerprint != previous_fingerprint {
                    if recommendations.is_empty() {
                        tracing::info!("Adaptive scheduling recommendations cleared");
                    } else {
                        tracing::info!(recommendations = ?recommendations, "Adaptive scheduling recommendations changed");
                    }
                    previous_fingerprint = fingerprint;
                }
            }
        }
    }
}

impl DiagnosticsWindow {
    fn from_snapshots(
        previous: Option<&DiagnosticsSnapshot>,
        current: &DiagnosticsSnapshot,
    ) -> Self {
        let previous_services = previous.map(service_snapshot_map).unwrap_or_default();
        let previous_generations = previous.map(generation_snapshot_map).unwrap_or_default();
        let previous_lanes = previous.map(lane_snapshot_map).unwrap_or_default();

        let mut services: Vec<_> = current
            .services
            .iter()
            .map(|service| {
                let previous = previous_services.get(&service.service_id);
                ServiceDiagnosticsWindow {
                    service_id: service.service_id,
                    service_name: service.service_name,
                    current_generation: service.current_generation,
                    runtime_lane: service.runtime_lane,
                    aggregate: aggregate_window(
                        previous.map(|snapshot| &snapshot.aggregate),
                        &service.aggregate,
                    ),
                }
            })
            .collect();
        services.sort_by_key(|service| service.service_id);

        let mut generations: Vec<_> = current
            .generations
            .iter()
            .map(|generation| {
                let previous =
                    previous_generations.get(&(generation.service_id, generation.generation));
                GenerationDiagnosticsWindow {
                    service_id: generation.service_id,
                    service_name: generation.service_name,
                    generation: generation.generation,
                    runtime_lane: generation.runtime_lane,
                    aggregate: aggregate_window(
                        previous.map(|snapshot| &snapshot.aggregate),
                        &generation.aggregate,
                    ),
                }
            })
            .collect();
        generations.sort_by_key(|generation| (generation.service_id, generation.generation));

        let mut lanes: Vec<_> = current
            .lanes
            .iter()
            .map(|lane| {
                let previous = previous_lanes.get(&lane.runtime_lane);
                RuntimeLaneWindow {
                    runtime_lane: lane.runtime_lane,
                    aggregate: aggregate_window(
                        previous.map(|snapshot| &snapshot.aggregate),
                        &lane.aggregate,
                    ),
                }
            })
            .collect();
        lanes.sort_by_key(|lane| lane_sort_key(lane.runtime_lane));

        Self {
            has_baseline: previous.is_some(),
            services,
            generations,
            lanes,
        }
    }
}

fn service_snapshot_map(
    snapshot: &DiagnosticsSnapshot,
) -> HashMap<ServiceId, &ServiceDiagnosticsSnapshot> {
    snapshot
        .services
        .iter()
        .map(|service| (service.service_id, service))
        .collect()
}

fn generation_snapshot_map(
    snapshot: &DiagnosticsSnapshot,
) -> HashMap<(ServiceId, u64), &GenerationDiagnosticsSnapshot> {
    snapshot
        .generations
        .iter()
        .map(|generation| ((generation.service_id, generation.generation), generation))
        .collect()
}

fn lane_snapshot_map(snapshot: &DiagnosticsSnapshot) -> HashMap<RuntimeLane, &RuntimeLaneSnapshot> {
    snapshot
        .lanes
        .iter()
        .map(|lane| (lane.runtime_lane, lane))
        .collect()
}

fn aggregate_window(
    previous: Option<&DiagnosticsAggregateSnapshot>,
    current: &DiagnosticsAggregateSnapshot,
) -> DiagnosticsAggregateWindow {
    DiagnosticsAggregateWindow {
        service_sleep: observation_window(
            previous.map(|snapshot| &snapshot.service_sleep),
            &current.service_sleep,
        ),
        runtime_probe: observation_window(
            previous.map(|snapshot| &snapshot.runtime_probe),
            &current.runtime_probe,
        ),
        lifecycle: lifecycle_window(
            previous.map(|snapshot| &snapshot.lifecycle),
            &current.lifecycle,
        ),
    }
}

fn observation_window(
    previous: Option<&ObservationStatsSnapshot>,
    current: &ObservationStatsSnapshot,
) -> ObservationWindow {
    let completed = delta(
        previous.map(|snapshot| snapshot.completed),
        current.completed,
    );
    let total_drift_ms = delta(
        previous.map(|snapshot| snapshot.total_drift_ms),
        current.total_drift_ms,
    );
    let proven_max_drift_ms = match previous {
        Some(previous) if completed > 0 && current.max_drift_ms > previous.max_drift_ms => {
            Some(current.max_drift_ms)
        }
        None if completed > 0 => Some(current.max_drift_ms),
        _ => None,
    };

    ObservationWindow {
        completed,
        interrupted: delta(
            previous.map(|snapshot| snapshot.interrupted),
            current.interrupted,
        ),
        total_requested_ms: delta(
            previous.map(|snapshot| snapshot.total_requested_ms),
            current.total_requested_ms,
        ),
        total_elapsed_ms: delta(
            previous.map(|snapshot| snapshot.total_elapsed_ms),
            current.total_elapsed_ms,
        ),
        total_drift_ms,
        proven_max_drift_ms,
        last_drift_ms: if completed > 0 {
            current.last_drift_ms
        } else {
            0
        },
        avg_drift_ms: total_drift_ms.checked_div(completed).unwrap_or(0),
    }
}

fn lifecycle_window(
    previous: Option<&LifecycleStatsSnapshot>,
    current: &LifecycleStatsSnapshot,
) -> LifecycleWindow {
    LifecycleWindow {
        reload_requested: delta(
            previous.map(|snapshot| snapshot.reload_requested),
            current.reload_requested,
        ),
        reload_exit: delta(
            previous.map(|snapshot| snapshot.reload_exit),
            current.reload_exit,
        ),
        restart: delta(previous.map(|snapshot| snapshot.restart), current.restart),
        backoff_restart: delta(
            previous.map(|snapshot| snapshot.backoff_restart),
            current.backoff_restart,
        ),
        rate_limited_restart: delta(
            previous.map(|snapshot| snapshot.rate_limited_restart),
            current.rate_limited_restart,
        ),
        terminated: delta(
            previous.map(|snapshot| snapshot.terminated),
            current.terminated,
        ),
        normal_exit: delta(
            previous.map(|snapshot| snapshot.normal_exit),
            current.normal_exit,
        ),
        recoverable_error: delta(
            previous.map(|snapshot| snapshot.recoverable_error),
            current.recoverable_error,
        ),
        panic: delta(previous.map(|snapshot| snapshot.panic), current.panic),
        fatal_service_error: delta(
            previous.map(|snapshot| snapshot.fatal_service_error),
            current.fatal_service_error,
        ),
        provider_init_error: delta(
            previous.map(|snapshot| snapshot.provider_init_error),
            current.provider_init_error,
        ),
        shutdown: delta(previous.map(|snapshot| snapshot.shutdown), current.shutdown),
        isolated_startup_failure: delta(
            previous.map(|snapshot| snapshot.isolated_startup_failure),
            current.isolated_startup_failure,
        ),
        last_policy_delay_ms: current.last_policy_delay_ms,
        last_effective_restart_delay_ms: current.last_effective_restart_delay_ms,
    }
}

fn recommendation_fingerprint(
    recommendations: &[SchedulingRecommendation],
) -> Vec<RecommendationFingerprint> {
    recommendations
        .iter()
        .map(|recommendation| {
            (
                target_fingerprint(&recommendation.target),
                recommendation.current_lane,
                recommendation.action,
                recommendation.reason,
                recommendation.confidence,
                observation_fingerprint(&recommendation.observation),
            )
        })
        .collect()
}

type RecommendationFingerprint = (
    TargetFingerprint,
    Option<RuntimeLane>,
    SchedulingRecommendationAction,
    SchedulingRecommendationReason,
    RecommendationConfidence,
    ObservationFingerprint,
);

type TargetFingerprint = (
    u8,
    Option<RuntimeLane>,
    Option<ServiceId>,
    Option<&'static str>,
    Option<u64>,
);

type ObservationFingerprint = (
    (
        u64,
        u64,
        u64,
        u64,
        u64,
        Option<u64>,
        u64,
        u64,
        u64,
        u64,
        u64,
        u64,
    ),
    (u64, u64, u64, u64, u64, u64, u64, u64, u64, u64, u64),
);

fn target_fingerprint(target: &SchedulingRecommendationTarget) -> TargetFingerprint {
    match target {
        SchedulingRecommendationTarget::RuntimeLane(lane) => (0, Some(*lane), None, None, None),
        SchedulingRecommendationTarget::Service {
            service_id,
            service_name,
            current_generation,
        } => (
            1,
            None,
            Some(*service_id),
            Some(*service_name),
            Some(*current_generation),
        ),
        SchedulingRecommendationTarget::Generation {
            service_id,
            service_name,
            generation,
            runtime_lane,
        } => (
            2,
            Some(*runtime_lane),
            Some(*service_id),
            Some(*service_name),
            Some(*generation),
        ),
    }
}

fn observation_fingerprint(observation: &RecommendationObservation) -> ObservationFingerprint {
    (
        (
            observation.completed_samples,
            observation.interrupted_samples,
            observation.total_requested_ms,
            observation.total_elapsed_ms,
            observation.total_drift_ms,
            observation.proven_max_drift_ms,
            observation.last_drift_ms,
            observation.avg_drift_ms,
            observation.reload_requested,
            observation.reload_exit,
            observation.restart,
            observation.backoff_restart,
        ),
        (
            observation.rate_limited_restart,
            observation.terminated,
            observation.normal_exit,
            observation.recoverable_error,
            observation.panic,
            observation.fatal_service_error,
            observation.provider_init_error,
            observation.shutdown,
            observation.isolated_startup_failure,
            observation.last_policy_delay_ms,
            observation.last_effective_restart_delay_ms,
        ),
    )
}

fn lane_sort_key(lane: RuntimeLane) -> u8 {
    match lane {
        RuntimeLane::Control => 0,
        RuntimeLane::Standard => 1,
        RuntimeLane::HighPriority => 2,
        RuntimeLane::Isolated => 3,
    }
}

fn delta(previous: Option<u64>, current: u64) -> u64 {
    previous.map_or(current, |previous| current.saturating_sub(previous))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::diagnostics::{
        GenerationExitKind, SleepExitReason, SleepObservation, SleepObservationSource,
    };
    use crate::models::ServiceId;

    fn completed_service_sleep(drift_ms: u64) -> SleepObservation {
        SleepObservation {
            source: SleepObservationSource::ServiceSleep,
            reason: SleepExitReason::Completed,
            requested: std::time::Duration::from_millis(10),
            elapsed: std::time::Duration::from_millis(10 + drift_ms),
            drift: std::time::Duration::from_millis(drift_ms),
        }
    }

    fn completed_runtime_probe(drift_ms: u64) -> SleepObservation {
        SleepObservation {
            source: SleepObservationSource::RuntimeProbe,
            reason: SleepExitReason::Completed,
            requested: std::time::Duration::from_millis(250),
            elapsed: std::time::Duration::from_millis(250 + drift_ms),
            drift: std::time::Duration::from_millis(drift_ms),
        }
    }

    fn quiet_observation() -> ObservationWindow {
        ObservationWindow {
            completed: MINIMUM_COMPLETED_SAMPLES,
            interrupted: 0,
            total_requested_ms: 750,
            total_elapsed_ms: 750,
            total_drift_ms: 0,
            proven_max_drift_ms: Some(0),
            last_drift_ms: 0,
            avg_drift_ms: 0,
        }
    }

    fn drift_observation(completed: u64, avg_drift_ms: u64) -> ObservationWindow {
        let total_requested_ms = completed.saturating_mul(250);
        let total_drift_ms = completed.saturating_mul(avg_drift_ms);
        ObservationWindow {
            completed,
            interrupted: 0,
            total_requested_ms,
            total_elapsed_ms: total_requested_ms.saturating_add(total_drift_ms),
            total_drift_ms,
            proven_max_drift_ms: Some(avg_drift_ms),
            last_drift_ms: avg_drift_ms,
            avg_drift_ms,
        }
    }

    fn quiet_lifecycle() -> LifecycleWindow {
        LifecycleWindow {
            reload_requested: 0,
            reload_exit: 0,
            restart: 0,
            backoff_restart: 0,
            rate_limited_restart: 0,
            terminated: 0,
            normal_exit: 0,
            recoverable_error: 0,
            panic: 0,
            fatal_service_error: 0,
            provider_init_error: 0,
            shutdown: 0,
            isolated_startup_failure: 0,
            last_policy_delay_ms: 0,
            last_effective_restart_delay_ms: 0,
        }
    }

    fn aggregate(
        service_sleep: ObservationWindow,
        runtime_probe: ObservationWindow,
        lifecycle: LifecycleWindow,
    ) -> DiagnosticsAggregateWindow {
        DiagnosticsAggregateWindow {
            service_sleep,
            runtime_probe,
            lifecycle,
        }
    }

    fn lane_window(
        runtime_lane: RuntimeLane,
        runtime_probe: ObservationWindow,
    ) -> RuntimeLaneWindow {
        RuntimeLaneWindow {
            runtime_lane,
            aggregate: aggregate(quiet_observation(), runtime_probe, quiet_lifecycle()),
        }
    }

    fn lane_window_with_lifecycle(
        runtime_lane: RuntimeLane,
        lifecycle: LifecycleWindow,
    ) -> RuntimeLaneWindow {
        RuntimeLaneWindow {
            runtime_lane,
            aggregate: aggregate(quiet_observation(), quiet_observation(), lifecycle),
        }
    }

    fn service_window(
        service_id: usize,
        runtime_lane: RuntimeLane,
        service_sleep: ObservationWindow,
    ) -> ServiceDiagnosticsWindow {
        ServiceDiagnosticsWindow {
            service_id: ServiceId::new(service_id),
            service_name: "worker",
            current_generation: 1,
            runtime_lane,
            aggregate: aggregate(service_sleep, quiet_observation(), quiet_lifecycle()),
        }
    }

    fn service_window_with_lifecycle(
        service_id: usize,
        runtime_lane: RuntimeLane,
        service_sleep: ObservationWindow,
        lifecycle: LifecycleWindow,
    ) -> ServiceDiagnosticsWindow {
        ServiceDiagnosticsWindow {
            service_id: ServiceId::new(service_id),
            service_name: "worker",
            current_generation: 1,
            runtime_lane,
            aggregate: aggregate(service_sleep, quiet_observation(), lifecycle),
        }
    }

    fn diagnostics_window(
        lanes: Vec<RuntimeLaneWindow>,
        services: Vec<ServiceDiagnosticsWindow>,
    ) -> DiagnosticsWindow {
        DiagnosticsWindow {
            has_baseline: true,
            services,
            generations: Vec::new(),
            lanes,
        }
    }

    #[test]
    fn sampler_reports_first_snapshot_without_baseline() {
        let store = crate::core::diagnostics::DiagnosticsStore::new();
        store.record_lane_observation(RuntimeLane::Standard, completed_runtime_probe(5));
        let mut sampler = DiagnosticsSampler::new();

        let window = sampler.sample(store.snapshot());

        assert!(!window.has_baseline);
        let standard = window
            .lanes
            .iter()
            .find(|lane| lane.runtime_lane == RuntimeLane::Standard);
        assert_eq!(
            standard.map(|lane| lane.aggregate.runtime_probe.completed),
            Some(1)
        );
    }

    #[test]
    fn sampler_computes_lane_window_delta() {
        let store = crate::core::diagnostics::DiagnosticsStore::new();
        store.record_lane_observation(RuntimeLane::Standard, completed_runtime_probe(5));
        let mut sampler = DiagnosticsSampler::new();
        sampler.sample(store.snapshot());

        store.record_lane_observation(RuntimeLane::Standard, completed_runtime_probe(15));
        store.record_lane_observation(RuntimeLane::Control, completed_runtime_probe(3));
        let window = sampler.sample(store.snapshot());

        assert!(window.has_baseline);
        let standard = window
            .lanes
            .iter()
            .find(|lane| lane.runtime_lane == RuntimeLane::Standard);
        assert_eq!(
            standard.map(|lane| &lane.aggregate.runtime_probe),
            Some(&ObservationWindow {
                completed: 1,
                interrupted: 0,
                total_requested_ms: 250,
                total_elapsed_ms: 265,
                total_drift_ms: 15,
                proven_max_drift_ms: Some(15),
                last_drift_ms: 15,
                avg_drift_ms: 15,
            })
        );
    }

    #[test]
    fn sampler_uses_saturating_delta_for_reset_snapshots() {
        let store = crate::core::diagnostics::DiagnosticsStore::new();
        store.record_lane_observation(RuntimeLane::Standard, completed_runtime_probe(20));
        let mut sampler = DiagnosticsSampler::new();
        sampler.sample(store.snapshot());

        let reset_store = crate::core::diagnostics::DiagnosticsStore::new();
        reset_store.record_lane_observation(RuntimeLane::Standard, completed_runtime_probe(5));
        let window = sampler.sample(reset_store.snapshot());

        let standard = window
            .lanes
            .iter()
            .find(|lane| lane.runtime_lane == RuntimeLane::Standard);
        assert_eq!(
            standard.map(|lane| lane.aggregate.runtime_probe.completed),
            Some(0)
        );
        assert_eq!(
            standard.map(|lane| lane.aggregate.runtime_probe.total_drift_ms),
            Some(0)
        );
    }

    #[test]
    fn sampler_computes_service_and_generation_windows() {
        let store = crate::core::diagnostics::DiagnosticsStore::new();
        let handle =
            store.register_generation(ServiceId::new(7), "worker", 1, RuntimeLane::Standard);
        handle.record_sleep_observation(completed_service_sleep(4));
        let mut sampler = DiagnosticsSampler::new();
        sampler.sample(store.snapshot());

        handle.record_sleep_observation(completed_service_sleep(8));
        handle.record_restart(
            crate::core::diagnostics::RestartDecisionKind::BackoffRecoverableError,
            std::time::Duration::from_millis(10),
            std::time::Duration::from_millis(20),
            true,
        );
        handle.record_exit(GenerationExitKind::RecoverableError);
        let window = sampler.sample(store.snapshot());

        let service = window
            .services
            .iter()
            .find(|service| service.service_id == ServiceId::new(7));
        assert_eq!(
            service.map(|service| service.aggregate.service_sleep.completed),
            Some(1)
        );
        assert_eq!(
            service.map(|service| service.aggregate.lifecycle.rate_limited_restart),
            Some(1)
        );

        let generation = window
            .generations
            .iter()
            .find(|generation| generation.service_id == ServiceId::new(7));
        assert_eq!(
            generation.map(|generation| generation.aggregate.service_sleep.avg_drift_ms),
            Some(8)
        );
        assert_eq!(
            generation.map(|generation| generation.aggregate.lifecycle.recoverable_error),
            Some(1)
        );
    }

    #[test]
    fn observation_window_reports_low_samples() {
        let window = ObservationWindow {
            completed: 1,
            interrupted: 0,
            total_requested_ms: 10,
            total_elapsed_ms: 12,
            total_drift_ms: 2,
            proven_max_drift_ms: Some(2),
            last_drift_ms: 2,
            avg_drift_ms: 2,
        };

        assert!(window.is_low_sample(2));
        assert!(!window.is_low_sample(1));
    }

    #[test]
    fn evaluator_ignores_first_window_without_baseline() {
        let evaluator = SchedulingPolicyEvaluator::default();
        let window = DiagnosticsWindow {
            has_baseline: false,
            services: vec![service_window(
                1,
                RuntimeLane::Standard,
                drift_observation(MINIMUM_COMPLETED_SAMPLES, HIGH_AVG_DRIFT_MS),
            )],
            generations: Vec::new(),
            lanes: vec![lane_window(
                RuntimeLane::Standard,
                drift_observation(MINIMUM_COMPLETED_SAMPLES, HIGH_AVG_DRIFT_MS),
            )],
        };

        assert!(evaluator.evaluate(&window).is_empty());
    }

    #[test]
    fn evaluator_suppresses_low_sample_lane_pressure() {
        let evaluator = SchedulingPolicyEvaluator::default();
        let window = diagnostics_window(
            vec![lane_window(
                RuntimeLane::Standard,
                drift_observation(MINIMUM_COMPLETED_SAMPLES - 1, HIGH_AVG_DRIFT_MS * 2),
            )],
            Vec::new(),
        );

        assert!(evaluator.evaluate(&window).is_empty());
    }

    #[test]
    fn standard_lane_drift_produces_lane_pressure_recommendation() {
        let evaluator = SchedulingPolicyEvaluator::default();
        let window = diagnostics_window(
            vec![lane_window(
                RuntimeLane::Standard,
                drift_observation(MINIMUM_COMPLETED_SAMPLES, HIGH_AVG_DRIFT_MS),
            )],
            Vec::new(),
        );

        let recommendations = evaluator.evaluate(&window);

        assert_eq!(recommendations.len(), 1);
        assert_eq!(
            recommendations[0].target,
            SchedulingRecommendationTarget::RuntimeLane(RuntimeLane::Standard)
        );
        assert_eq!(
            recommendations[0].action,
            SchedulingRecommendationAction::Observe
        );
        assert_eq!(
            recommendations[0].reason,
            SchedulingRecommendationReason::StandardLanePressure
        );
    }

    #[test]
    fn standard_service_drift_can_consider_isolation_without_blockers() {
        let evaluator = SchedulingPolicyEvaluator::default();
        let window = diagnostics_window(
            Vec::new(),
            vec![service_window(
                1,
                RuntimeLane::Standard,
                drift_observation(MINIMUM_COMPLETED_SAMPLES, HIGH_AVG_DRIFT_MS),
            )],
        );

        let recommendations = evaluator.evaluate(&window);

        assert_eq!(recommendations.len(), 1);
        assert_eq!(
            recommendations[0].target,
            SchedulingRecommendationTarget::Service {
                service_id: ServiceId::new(1),
                service_name: "worker",
                current_generation: 1,
            }
        );
        assert_eq!(
            recommendations[0].action,
            SchedulingRecommendationAction::ConsiderIsolation
        );
        assert_eq!(
            recommendations[0].reason,
            SchedulingRecommendationReason::StandardServiceSleepDrift
        );
    }

    #[test]
    fn standard_lane_pressure_keeps_service_drift_advisory() {
        let evaluator = SchedulingPolicyEvaluator::default();
        let window = diagnostics_window(
            vec![lane_window(
                RuntimeLane::Standard,
                drift_observation(MINIMUM_COMPLETED_SAMPLES, HIGH_AVG_DRIFT_MS),
            )],
            vec![service_window(
                1,
                RuntimeLane::Standard,
                drift_observation(MINIMUM_COMPLETED_SAMPLES, HIGH_AVG_DRIFT_MS),
            )],
        );

        let recommendations = evaluator.evaluate(&window);
        let service = recommendations.iter().find(|recommendation| {
            matches!(
                recommendation.target,
                SchedulingRecommendationTarget::Service { .. }
            )
        });

        assert_eq!(
            service.map(|recommendation| recommendation.action),
            Some(SchedulingRecommendationAction::Observe)
        );
        assert!(
            !recommendations
                .iter()
                .any(|recommendation| recommendation.action
                    == SchedulingRecommendationAction::ConsiderIsolation)
        );
    }

    #[test]
    fn control_lane_drift_investigates_control_plane() {
        let evaluator = SchedulingPolicyEvaluator::default();
        let window = diagnostics_window(
            vec![lane_window(
                RuntimeLane::Control,
                drift_observation(MINIMUM_COMPLETED_SAMPLES, HIGH_AVG_DRIFT_MS),
            )],
            Vec::new(),
        );

        let recommendations = evaluator.evaluate(&window);

        assert_eq!(recommendations.len(), 1);
        assert_eq!(
            recommendations[0].action,
            SchedulingRecommendationAction::InvestigateControlPlane
        );
        assert_eq!(
            recommendations[0].reason,
            SchedulingRecommendationReason::ControlPlanePressure
        );
    }

    #[test]
    fn high_priority_drift_warns_without_inbound_migration() {
        let evaluator = SchedulingPolicyEvaluator::default();
        let window = diagnostics_window(
            vec![lane_window(
                RuntimeLane::HighPriority,
                drift_observation(MINIMUM_COMPLETED_SAMPLES, HIGH_AVG_DRIFT_MS),
            )],
            Vec::new(),
        );

        let recommendations = evaluator.evaluate(&window);

        assert_eq!(recommendations.len(), 1);
        assert_eq!(
            recommendations[0].reason,
            SchedulingRecommendationReason::HighPrioritySaturation
        );
        assert!(
            !recommendations
                .iter()
                .any(|recommendation| recommendation.action
                    == SchedulingRecommendationAction::ConsiderIsolation)
        );
    }

    #[test]
    fn isolated_pressure_suppresses_additional_isolation_suggestions() {
        let evaluator = SchedulingPolicyEvaluator::default();
        let mut isolated_lifecycle = quiet_lifecycle();
        isolated_lifecycle.isolated_startup_failure = 1;
        let window = diagnostics_window(
            vec![lane_window_with_lifecycle(
                RuntimeLane::Isolated,
                isolated_lifecycle,
            )],
            vec![service_window(
                1,
                RuntimeLane::Standard,
                drift_observation(MINIMUM_COMPLETED_SAMPLES, HIGH_AVG_DRIFT_MS),
            )],
        );

        let recommendations = evaluator.evaluate(&window);

        assert!(recommendations.iter().any(|recommendation| {
            recommendation.reason == SchedulingRecommendationReason::IsolatedResourcePressure
        }));
        assert!(
            !recommendations
                .iter()
                .any(|recommendation| recommendation.action
                    == SchedulingRecommendationAction::ConsiderIsolation)
        );
    }

    #[test]
    fn restart_instability_keeps_service_on_current_lane() {
        let evaluator = SchedulingPolicyEvaluator::default();
        let mut lifecycle = quiet_lifecycle();
        lifecycle.rate_limited_restart = 1;
        let window = diagnostics_window(
            Vec::new(),
            vec![service_window_with_lifecycle(
                1,
                RuntimeLane::Standard,
                drift_observation(MINIMUM_COMPLETED_SAMPLES, HIGH_AVG_DRIFT_MS),
                lifecycle,
            )],
        );

        let recommendations = evaluator.evaluate(&window);

        assert_eq!(recommendations.len(), 1);
        assert_eq!(
            recommendations[0].action,
            SchedulingRecommendationAction::KeepCurrentLane
        );
        assert_eq!(
            recommendations[0].reason,
            SchedulingRecommendationReason::LifecycleInstability
        );
    }

    #[test]
    fn recommendation_fingerprint_is_stable_and_detects_changes() {
        let first = SchedulingRecommendation {
            target: SchedulingRecommendationTarget::RuntimeLane(RuntimeLane::Standard),
            current_lane: Some(RuntimeLane::Standard),
            action: SchedulingRecommendationAction::Observe,
            reason: SchedulingRecommendationReason::StandardLanePressure,
            confidence: RecommendationConfidence::Medium,
            observation: RecommendationObservation::from_aggregate(
                &aggregate(
                    quiet_observation(),
                    drift_observation(MINIMUM_COMPLETED_SAMPLES, HIGH_AVG_DRIFT_MS),
                    quiet_lifecycle(),
                ),
                &drift_observation(MINIMUM_COMPLETED_SAMPLES, HIGH_AVG_DRIFT_MS),
            ),
        };
        let mut second = first.clone();
        second.observation.avg_drift_ms = HIGH_AVG_DRIFT_MS + 1;

        assert_eq!(
            recommendation_fingerprint(std::slice::from_ref(&first)),
            recommendation_fingerprint(std::slice::from_ref(&first))
        );
        assert_ne!(
            recommendation_fingerprint(&[first]),
            recommendation_fingerprint(&[second])
        );
    }

    #[tokio::test]
    async fn recommendation_loop_cancels_promptly() {
        let token = CancellationToken::new();
        token.cancel();
        let result = tokio::time::timeout(
            Duration::from_millis(50),
            run_adaptive_scheduling_recommendations(
                Arc::new(crate::core::diagnostics::DiagnosticsStore::new()),
                token,
            ),
        )
        .await;

        assert!(result.is_ok());
    }
}
