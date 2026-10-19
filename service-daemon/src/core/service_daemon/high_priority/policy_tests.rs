use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::Context;
use tracing_subscriber::{Layer, prelude::*};

use super::{
    HighPriorityPlacementDecisionKind, HighPriorityPlacementReason, HighPriorityRuntimePool,
};
use crate::core::diagnostics::{
    GenerationRegistration, RuntimeLane, SleepExitReason, SleepObservation, SleepObservationSource,
};
use crate::core::service_daemon::DaemonInstanceInner;
use crate::models::policy::HighPriorityRuntimeControl;
use crate::models::{HighPriorityShardId, ServiceInstanceId, ServiceScheduling};

#[derive(Clone, Default)]
struct PolicyEvents(Arc<Mutex<Vec<(Level, String)>>>);

impl<S: Subscriber> Layer<S> for PolicyEvents {
    fn on_event(&self, event: &Event<'_>, _: Context<'_, S>) {
        if event.metadata().target()
            != "service_daemon::core::service_daemon::high_priority::runtime"
        {
            return;
        }
        struct Fields(String);
        impl Visit for Fields {
            fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
                use std::fmt::Write;
                write!(&mut self.0, "{}={value:?} ", field.name()).unwrap();
            }
        }
        let mut fields = Fields(String::new());
        event.record(&mut fields);
        self.0
            .lock()
            .unwrap()
            .push((*event.metadata().level(), fields.0));
    }
}

impl PolicyEvents {
    fn debug_decisions(&self) -> Vec<String> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .filter(|(level, _)| *level == Level::DEBUG)
            .map(|(_, fields)| fields.clone())
            .collect()
    }
}

fn daemon() -> DaemonInstanceInner {
    let mut daemon = crate::ServiceDaemon::builder()
        .with_registry(
            crate::Registry::builder()
                .with_tag("__unit_high_priority_capacity_primary__")
                .build(),
        )
        .build_inner();
    daemon.high_priority_runtime_pool = HighPriorityRuntimePool::new(
        HighPriorityRuntimeControl::for_testing(),
        daemon.high_priority_capacity,
    );
    daemon.ensure_high_priority_runtime().unwrap();
    daemon
}

fn register_service(daemon: &DaemonInstanceInner) -> ServiceInstanceId {
    let instance = ServiceInstanceId::new(uuid::Uuid::from_u128(9100));
    let (shard, _) = daemon
        .high_priority_runtime_pool
        .state()
        .select_generation(instance, 1)
        .unwrap();
    daemon
        .diagnostics
        .register_generation_with_placement(GenerationRegistration {
            service_instance_id: instance,
            service_name: "policy_regression",
            generation: 1,
            declared_scheduling: ServiceScheduling::HighPriority,
            lane: RuntimeLane::HighPriority,
            high_priority_shard_id: Some(shard.shard_id),
            placement_decision: None,
        });
    daemon
        .resources
        .reload_signals
        .insert(instance, Arc::new(tokio::sync::Notify::new()));
    instance
}

fn probe(drift: Duration) -> SleepObservation {
    SleepObservation {
        source: SleepObservationSource::RuntimeProbe,
        reason: SleepExitReason::Completed,
        requested: Duration::from_millis(250),
        elapsed: Duration::from_millis(250) + drift,
        drift,
    }
}

fn pressured_sample(daemon: &DaemonInstanceInner, instance: ServiceInstanceId, at: Instant) {
    daemon.diagnostics.record_high_priority_sleep_for_test(
        instance,
        1,
        at,
        Duration::from_millis(20),
    );
}

#[test]
fn idle_policy_ticks_do_not_report_suppression() {
    let mut daemon = daemon();
    let events = PolicyEvents::default();
    let subscriber = tracing_subscriber::registry().with(events.clone());
    let base = Instant::now() + Duration::from_secs(1);
    tracing::subscriber::with_default(subscriber, || {
        for tick in 0..160 {
            daemon.evaluate_high_priority_runtime_policy_for_test(
                base + Duration::from_millis(tick * 250),
            );
        }
    });
    assert!(
        events.debug_decisions().is_empty(),
        "idle polling must not emit a DEBUG suppression event"
    );
    assert!(
        daemon
            .diagnostics
            .snapshot()
            .high_priority_placement_decisions
            .is_empty(),
        "idle polling must not consume decision history"
    );
    daemon.shutdown_high_priority_runtime();
}

#[test]
fn healthy_samples_do_not_report_insufficient_samples() {
    let mut daemon = daemon();
    let instance = register_service(&daemon);
    let events = PolicyEvents::default();
    let subscriber = tracing_subscriber::registry().with(events.clone());
    let base = Instant::now() + Duration::from_secs(1);
    tracing::subscriber::with_default(subscriber, || {
        for tick in 0..160 {
            let at = base + Duration::from_millis(tick * 250);
            daemon
                .diagnostics
                .record_high_priority_sleep_for_test(instance, 1, at, Duration::ZERO);
            daemon.evaluate_high_priority_runtime_policy_for_test(at);
        }
    });
    assert!(events.debug_decisions().is_empty());
    assert!(
        daemon
            .diagnostics
            .snapshot()
            .high_priority_placement_decisions
            .is_empty()
    );
    daemon.shutdown_high_priority_runtime();
}

#[test]
fn waiting_ticks_preserve_actual_intervention_history() {
    let mut daemon = daemon();
    let instance = register_service(&daemon);
    let base = Instant::now() + Duration::from_secs(1);
    daemon.diagnostics.record_high_priority_shard_observation(
        HighPriorityShardId(0),
        probe(Duration::from_millis(20)),
    );
    pressured_sample(&daemon, instance, base);
    daemon.evaluate_high_priority_runtime_policy_for_test(base);
    let before = daemon
        .diagnostics
        .snapshot()
        .high_priority_placement_decisions;
    assert_eq!(daemon.high_priority_runtime_pool.total_worker_threads(), 2);
    assert!(
        before
            .iter()
            .any(|decision| { decision.kind == HighPriorityPlacementDecisionKind::ScaleOut })
    );
    assert!(
        before
            .iter()
            .any(|decision| { decision.kind == HighPriorityPlacementDecisionKind::Rollover })
    );

    let events = PolicyEvents::default();
    tracing::subscriber::with_default(tracing_subscriber::registry().with(events.clone()), || {
        for tick in 1..=160 {
            daemon.evaluate_high_priority_runtime_policy_for_test(
                base + Duration::from_millis(tick * 250),
            );
        }
    });
    assert_eq!(
        daemon
            .diagnostics
            .snapshot()
            .high_priority_placement_decisions,
        before
    );
    assert!(events.debug_decisions().is_empty());
    daemon.shutdown_high_priority_runtime();
}

#[test]
fn suppression_is_recorded_once_until_reason_or_service_pressure_changes() {
    let mut daemon = daemon();
    let instance = register_service(&daemon);
    let base = Instant::now() + Duration::from_secs(1);
    daemon.high_priority_runtime_pool.scale_out(base).unwrap();
    for shard in [HighPriorityShardId(0), HighPriorityShardId(1)] {
        daemon
            .diagnostics
            .record_high_priority_shard_observation(shard, probe(Duration::from_millis(20)));
    }
    for lane in [RuntimeLane::Control, RuntimeLane::Standard] {
        daemon
            .diagnostics
            .record_lane_observation(lane, probe(Duration::from_millis(20)));
    }
    let events = PolicyEvents::default();
    tracing::subscriber::with_default(tracing_subscriber::registry().with(events.clone()), || {
        for tick in 1..=160 {
            let at = base + Duration::from_secs(tick);
            pressured_sample(&daemon, instance, at);
            daemon.evaluate_high_priority_runtime_policy_for_test(at);
            daemon.evaluate_high_priority_runtime_policy_for_test(at + Duration::from_millis(250));
        }
        for lane in [RuntimeLane::Control, RuntimeLane::Standard] {
            for _ in 0..32 {
                daemon
                    .diagnostics
                    .record_lane_observation(lane, probe(Duration::ZERO));
            }
        }
        for tick in 161..=320 {
            let at = base + Duration::from_secs(tick);
            pressured_sample(&daemon, instance, at);
            daemon.evaluate_high_priority_runtime_policy_for_test(at);
            daemon.evaluate_high_priority_runtime_policy_for_test(at + Duration::from_millis(250));
        }
        let healthy = base + Duration::from_secs(321);
        daemon.diagnostics.record_high_priority_sleep_for_test(
            instance,
            1,
            healthy,
            Duration::ZERO,
        );
        daemon.evaluate_high_priority_runtime_policy_for_test(healthy);
        let pressured = base + Duration::from_secs(322);
        pressured_sample(&daemon, instance, pressured);
        daemon.evaluate_high_priority_runtime_policy_for_test(pressured);
    });
    let reasons: Vec<_> = daemon
        .diagnostics
        .snapshot()
        .high_priority_placement_decisions
        .iter()
        .map(|decision| decision.reason)
        .collect();
    assert_eq!(
        reasons,
        [
            HighPriorityPlacementReason::GlobalPressure,
            HighPriorityPlacementReason::MaxCapacity,
            HighPriorityPlacementReason::MaxCapacity,
        ]
    );
    let debug = events.debug_decisions();
    assert_eq!(
        debug.len(),
        3,
        "waiting ticks must not reset suppression deduplication"
    );
    assert!(
        debug
            .iter()
            .all(|event| event.contains("service_instance_id"))
    );
    daemon.shutdown_high_priority_runtime();
}

#[test]
fn pending_intervention_expires_without_new_sleep_samples() {
    let mut daemon = daemon();
    let instance = register_service(&daemon);
    let base = Instant::now() + Duration::from_secs(1);
    daemon.diagnostics.record_high_priority_shard_observation(
        HighPriorityShardId(0),
        probe(Duration::from_millis(20)),
    );
    pressured_sample(&daemon, instance, base);
    daemon.evaluate_high_priority_runtime_policy_for_test(base);
    let state = daemon.high_priority_runtime_pool.state();
    assert!(!state.request_rollover(instance, 1, HighPriorityShardId(1)));

    let events = PolicyEvents::default();
    tracing::subscriber::with_default(tracing_subscriber::registry().with(events.clone()), || {
        daemon.evaluate_high_priority_runtime_policy_for_test(base + Duration::from_secs(121));
    });
    assert!(state.request_rollover(instance, 1, HighPriorityShardId(1)));
    assert!(
        events
            .0
            .lock()
            .unwrap()
            .iter()
            .any(|(level, fields)| { *level == Level::WARN && fields.contains("TimedOut") })
    );
    state.cancel_rollover(instance);
    daemon.shutdown_high_priority_runtime();
}

#[test]
fn fresh_samples_resume_intervention_after_shard_pressure_changes() {
    let mut daemon = daemon();
    let instance = register_service(&daemon);
    let base = Instant::now() + Duration::from_secs(1);
    pressured_sample(&daemon, instance, base);
    daemon.evaluate_high_priority_runtime_policy_for_test(base);
    assert_eq!(daemon.high_priority_runtime_pool.total_worker_threads(), 1);
    daemon.evaluate_high_priority_runtime_policy_for_test(base + Duration::from_millis(250));

    daemon.diagnostics.record_high_priority_shard_observation(
        HighPriorityShardId(0),
        probe(Duration::from_millis(20)),
    );
    let next = base + Duration::from_secs(1);
    pressured_sample(&daemon, instance, next);
    daemon.evaluate_high_priority_runtime_policy_for_test(next);
    assert_eq!(daemon.high_priority_runtime_pool.total_worker_threads(), 2);
    assert!(
        daemon
            .diagnostics
            .snapshot()
            .high_priority_placement_decisions
            .iter()
            .any(|decision| decision.kind == HighPriorityPlacementDecisionKind::Rollover)
    );
    daemon.shutdown_high_priority_runtime();
}
