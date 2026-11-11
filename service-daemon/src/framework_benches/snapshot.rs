use std::hint::black_box;

use criterion::{BenchmarkId, Criterion};

use super::observation::{PREFILL, completed_sleep, register};
use crate::ServiceScheduling;
use crate::core::diagnostics::{DiagnosticsStore, RuntimeLane};

pub(super) fn run(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("diagnostics_snapshot");
    for count in [1usize, 32, 256] {
        let store = DiagnosticsStore::new();
        for id in 1..=count {
            let handle = register(&store, id as u128, ServiceScheduling::Standard);
            for _ in 0..PREFILL {
                handle.record_sleep_observation(completed_sleep());
            }
        }
        let fixture = store.snapshot();
        assert_eq!(fixture.services.len(), count);
        assert_eq!(fixture.generations.len(), count);
        for service in &fixture.services {
            assert_eq!(service.current_generation, 1);
            assert_eq!(service.aggregate.service_sleep.completed, PREFILL);
        }
        for generation in &fixture.generations {
            assert_eq!(generation.generation, 1);
            assert_eq!(generation.aggregate.service_sleep.completed, PREFILL);
        }
        let lane = fixture
            .lanes
            .iter()
            .find(|l| l.runtime_lane == RuntimeLane::Standard)
            .unwrap();
        assert_eq!(
            lane.aggregate.service_sleep.completed,
            PREFILL * count as u64
        );
        drop(fixture);

        group.bench_with_input(BenchmarkId::from_parameter(count), &store, |b, store| {
            // Include allocation, aggregation, sorting, consuming and freeing the snapshot.
            b.iter(|| drop(black_box(store.snapshot())));
        });
    }
    group.finish();
}
