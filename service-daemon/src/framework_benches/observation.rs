use std::{hint::black_box, time::Duration};

use criterion::Criterion;

use crate::core::diagnostics::{
    DiagnosticsStore, GenerationDiagnosticsHandle, GenerationRegistration, RuntimeLane,
    SleepExitReason, SleepObservation, SleepObservationSource,
};
use crate::{ServiceInstanceId, ServiceScheduling};

pub(super) const PREFILL: u64 = 128;

pub(super) fn completed_sleep() -> SleepObservation {
    SleepObservation {
        source: SleepObservationSource::ServiceSleep,
        reason: SleepExitReason::Completed,
        requested: Duration::from_millis(1),
        elapsed: Duration::from_millis(2),
        drift: Duration::from_millis(1),
    }
}

pub(super) fn register(
    store: &DiagnosticsStore,
    id: u128,
    scheduling: ServiceScheduling,
) -> GenerationDiagnosticsHandle {
    store.register_generation_with_placement(GenerationRegistration {
        service_instance_id: ServiceInstanceId::new(uuid::Uuid::from_u128(id)),
        service_name: "criterion_fixture",
        generation: 1,
        declared_scheduling: scheduling,
        lane: RuntimeLane::from(scheduling),
        #[cfg(feature = "high-priority")]
        high_priority_shard_id: (scheduling == ServiceScheduling::HighPriority)
            .then_some(crate::HighPriorityShardId(0)),
        #[cfg(feature = "high-priority")]
        placement_decision: None,
    })
}

pub(super) fn run(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("observation");
    let cases = [
        ("standard_steady", ServiceScheduling::Standard),
        #[cfg(feature = "high-priority")]
        ("high_priority_steady", ServiceScheduling::HighPriority),
    ];
    for (name, scheduling) in cases {
        let store = DiagnosticsStore::new();
        let handle = register(&store, 1, scheduling);
        #[cfg(feature = "high-priority")]
        if scheduling == ServiceScheduling::HighPriority {
            // Setup only: the real window excludes sleeps whose inferred start
            // precedes registration. Let this synthetic 2 ms sleep fit wholly
            // inside the generation before checking the window via its real API.
            std::thread::sleep(completed_sleep().elapsed);
        }
        for _ in 0..PREFILL {
            handle.record_sleep_observation(completed_sleep());
        }
        // Fixture verification is outside Criterion's timing loop, including smoke mode.
        let snapshot = store.snapshot();
        assert_eq!(
            snapshot.generations[0].aggregate.service_sleep.completed,
            PREFILL
        );
        assert_eq!(
            snapshot.services[0].aggregate.service_sleep.completed,
            PREFILL
        );
        let lane = snapshot
            .lanes
            .iter()
            .find(|l| l.runtime_lane == RuntimeLane::from(scheduling))
            .unwrap();
        assert_eq!(lane.aggregate.service_sleep.completed, PREFILL);
        drop(snapshot);
        #[cfg(feature = "high-priority")]
        if scheduling == ServiceScheduling::HighPriority {
            let window = || {
                store
                    .high_priority_sleep_sample(
                        ServiceInstanceId::new(uuid::Uuid::from_u128(1)),
                        1,
                        std::time::Instant::now(),
                        None,
                        Duration::ZERO,
                    )
                    .unwrap()
                    .window
            };
            let before = window();
            assert_eq!(before.completed, PREFILL);
            handle.record_sleep_observation(completed_sleep());
            let after = window();
            assert_eq!(after.completed, PREFILL);
            assert_eq!(after.sequence, before.sequence + 1);
            assert_eq!(after.mean_drift_ns, 1_000_000);
        }
        group.bench_function(name, |b| {
            b.iter(|| handle.record_sleep_observation(black_box(completed_sleep())));
        });
    }
    group.finish();
}
