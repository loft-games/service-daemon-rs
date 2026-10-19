//! Fixtures shared by the internal Criterion executable and its smoke test.

use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::{HighPriorityCapacityPlan, HighPriorityRuntimePool};
use crate::core::diagnostics::{
    DiagnosticsStore, GenerationRegistration, RuntimeLane, SleepExitReason, SleepObservation,
    SleepObservationSource,
};
use crate::core::service_daemon::DaemonInstanceInner;
use crate::models::policy::HighPriorityRuntimeControl;
use crate::{HighPriorityShardId, ServiceInstanceId, ServiceScheduling};

pub(crate) struct PolicyFixture {
    daemon: DaemonInstanceInner,
    now: Instant,
    active: usize,
}

impl PolicyFixture {
    pub(crate) fn new(active: usize, standard_history: usize) -> Self {
        let parallelism = NonZeroUsize::new(1);
        let capacity = HighPriorityCapacityPlan::from_entry_count(1, parallelism);
        let diagnostics = Arc::new(DiagnosticsStore::new());
        // Avoid the link-time registry: the internal benchmark executable also
        // links the public crate, which gives linkme two SERVICE_REGISTRY slices.
        let mut daemon = DaemonInstanceInner {
            services: Vec::new(),
            instance_registry: Arc::default(),
            removing_instances: Arc::default(),
            stopping_instances: Arc::default(),
            running_tasks: Arc::default(),
            restart_policy: Default::default(),
            cancellation_token: tokio_util::sync::CancellationToken::new(),
            control_runtime: None,
            standard_runtime: None,
            high_priority_capacity: capacity,
            high_priority_runtime_pool: HighPriorityRuntimePool::new_with_parallelism(
                HighPriorityRuntimeControl::default(),
                capacity,
                parallelism,
            ),
            runtime_probe_tasks: Vec::new(),
            adaptive_recommendation_task: None,
            high_priority_policy_task: None,
            scheduling_advisory_profile: Default::default(),
            external_cancel_token: None,
            resources: crate::core::context::DaemonResources::new_with_diagnostics(
                diagnostics.clone(),
            ),
            diagnostics,
            isolated_startup_permits: Arc::new(tokio::sync::Semaphore::new(4)),
            startup_gate: Arc::default(),
        };
        daemon
            .high_priority_runtime_pool
            .prepare_initial_runtime()
            .unwrap();
        let mut handles = Vec::new();
        for index in 0..active {
            let instance = ServiceInstanceId::new(uuid::Uuid::from_u128(index as u128 + 1));
            let (shard, _) = daemon
                .high_priority_runtime_pool
                .state()
                .select_generation(instance, 1)
                .unwrap();
            handles.push(daemon.diagnostics.register_generation_with_placement(
                GenerationRegistration {
                    service_instance_id: instance,
                    service_name: "policy_bench_high_priority",
                    generation: 1,
                    declared_scheduling: ServiceScheduling::HighPriority,
                    lane: RuntimeLane::HighPriority,
                    high_priority_shard_id: Some(shard.shard_id),
                    placement_decision: None,
                },
            ));
            daemon
                .resources
                .status_plane
                .insert(instance, crate::ServiceStatus::Healthy);
        }
        for index in 0..standard_history {
            let instance = ServiceInstanceId::new(uuid::Uuid::from_u128(10_000 + index as u128));
            for generation in 1..=8 {
                let handle =
                    daemon
                        .diagnostics
                        .register_generation_with_placement(GenerationRegistration {
                            service_instance_id: instance,
                            service_name: "policy_bench_standard_history",
                            generation,
                            declared_scheduling: ServiceScheduling::Standard,
                            lane: RuntimeLane::Standard,
                            high_priority_shard_id: None,
                            placement_decision: None,
                        });
                for _ in 0..128 {
                    handle.record_sleep_observation(healthy_sleep());
                }
            }
        }
        // Allow the production settling boundary to pass before filling the window.
        if active > 0 {
            std::thread::sleep(Duration::from_millis(2_010));
        }
        for handle in handles {
            for _ in 0..128 {
                handle.record_sleep_observation(healthy_sleep());
            }
        }
        for _ in 0..32 {
            daemon.diagnostics.record_high_priority_shard_observation(
                HighPriorityShardId(0),
                SleepObservation {
                    source: SleepObservationSource::RuntimeProbe,
                    ..healthy_sleep()
                },
            );
        }
        let now = Instant::now();
        if active > 0 {
            let first = daemon
                .diagnostics
                .high_priority_sleep_sample(
                    ServiceInstanceId::new(uuid::Uuid::from_u128(1)),
                    1,
                    now,
                    None,
                    Duration::from_secs(2),
                    0,
                )
                .unwrap();
            assert_eq!(first.window.completed, 128);
            assert_eq!(first.window.mean_drift_ns, 1_000_000);
        }
        let snapshot = daemon.diagnostics.snapshot();
        assert_eq!(snapshot.services.len(), active + standard_history);
        assert_eq!(snapshot.generations.len(), active + standard_history * 8);
        let mut fixture = Self {
            daemon,
            now,
            active,
        };
        // Seed cursors and decision history before timing the unchanged-input path.
        for _ in 0..128 {
            fixture.tick();
        }
        fixture.verify();
        fixture
    }

    pub(crate) fn tick(&mut self) {
        // The fixed observation time keeps this an unchanged-input measurement;
        // it does not model wall-clock timers, task execution, or fresh samples.
        self.daemon.evaluate_high_priority_runtime_policy(self.now);
    }

    pub(crate) fn verify(&self) {
        let shards = self.daemon.high_priority_runtime_pool.snapshot();
        assert_eq!(shards.len(), 1);
        assert_eq!(shards[0].active_generations, self.active);
        assert_eq!(shards[0].assigned_instances, self.active);
        assert!(
            self.daemon
                .high_priority_runtime_pool
                .state()
                .pending_rollovers
                .is_empty()
        );
    }
}

impl Drop for PolicyFixture {
    fn drop(&mut self) {
        self.daemon.shutdown_high_priority_runtime();
    }
}

fn healthy_sleep() -> SleepObservation {
    SleepObservation {
        source: SleepObservationSource::ServiceSleep,
        reason: SleepExitReason::Completed,
        requested: Duration::from_millis(1),
        elapsed: Duration::from_millis(2),
        drift: Duration::from_millis(1),
    }
}

#[test]
fn policy_benchmark_fixture_keeps_healthy_generations_in_place() {
    let mut fixture = PolicyFixture::new(1, 2);
    fixture.tick();
    fixture.verify();
}
