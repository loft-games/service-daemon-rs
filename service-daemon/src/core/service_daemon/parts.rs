use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use tokio::runtime::Handle;
use tokio::sync::{Mutex, Semaphore};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::ProviderDependencyWatchSet;
use crate::core::diagnostics::DiagnosticsStore;
use crate::models::{
    ServiceFn, ServiceInstanceId, ServiceInstanceRecord, ServiceInvocationContext,
    ServiceScheduling,
};

use super::super::context::DaemonResources;
#[cfg(feature = "high-priority")]
use super::high_priority::{
    HighPriorityPlacementDecision, HighPriorityPlacementDecisionKind, HighPriorityPlacementReason,
    HighPriorityRuntimePoolState,
};
use super::policy::RestartPolicy;
#[cfg(feature = "high-priority")]
use crate::models::HighPriorityShardId;

pub(super) struct ServiceSupervisorParts {
    pub service_instance_id: ServiceInstanceId,
    pub name: &'static str,
    pub run: ServiceFn,
    pub invocation_context: ServiceInvocationContext,
    pub watcher: Option<fn() -> ProviderDependencyWatchSet>,
    pub policy: RestartPolicy,
    pub scheduling: ServiceScheduling,
    pub body_lanes: BodyExecutionLanes,
    pub body_lane_resolver: BodyLaneResolver,
    pub resources: Arc<DaemonResources>,
    pub diagnostics: Arc<DiagnosticsStore>,
    pub isolated_startup_permits: Arc<Semaphore>,
    pub cancellation_token: CancellationToken,
    pub daemon_token: CancellationToken,
}

#[derive(Clone)]
pub(super) enum SupervisorSpawnLane {
    Control(Handle),
}

#[derive(Clone)]
pub(super) struct BodyExecutionLanes {
    pub standard: Handle,
    #[cfg(feature = "high-priority")]
    pub high_priority: Option<Arc<HighPriorityRuntimePoolState>>,
}

impl BodyExecutionLanes {
    pub(super) fn resolve(
        &self,
        service_instance_id: ServiceInstanceId,
        generation: u64,
        scheduling: ServiceScheduling,
    ) -> Option<BodyExecutionLane> {
        let _ = (service_instance_id, generation);
        match scheduling {
            ServiceScheduling::Standard => Some(BodyExecutionLane::Standard(self.standard.clone())),
            #[cfg(feature = "high-priority")]
            ServiceScheduling::HighPriority => {
                let Some(pool) = self.high_priority.as_ref() else {
                    return Some(BodyExecutionLane::UnavailableHighPriority(
                        HighPriorityPlacementDecision {
                            shard_id: None,
                            kind: HighPriorityPlacementDecisionKind::Suppressed,
                            reason: HighPriorityPlacementReason::NoHighPriorityRuntime,
                        },
                    ));
                };
                pool.select_generation(service_instance_id, generation)
                    .map(|(shard, decision)| BodyExecutionLane::HighPriority {
                        shard_id: shard.shard_id,
                        runtime: shard.handle,
                        decision,
                        accounting: pool.clone(),
                    })
            }
            ServiceScheduling::Isolated => Some(BodyExecutionLane::Isolated),
        }
    }
}

#[cfg(test)]
type BodyLaneOverrideResolver =
    Arc<dyn Fn(ServiceInstanceId, u64, ServiceScheduling) -> ServiceScheduling + Send + Sync>;

#[derive(Clone, Default)]
pub(super) struct BodyLaneResolver {
    #[cfg(test)]
    override_resolver: Option<BodyLaneOverrideResolver>,
}

impl BodyLaneResolver {
    pub(super) fn resolve(
        &self,
        service_instance_id: ServiceInstanceId,
        generation: u64,
        declared_scheduling: ServiceScheduling,
    ) -> ServiceScheduling {
        #[cfg(test)]
        if let Some(resolver) = &self.override_resolver {
            return resolver(service_instance_id, generation, declared_scheduling);
        }

        let _ = (service_instance_id, generation);
        declared_scheduling
    }

    #[cfg(test)]
    pub(super) fn with_override(
        resolver: impl Fn(ServiceInstanceId, u64, ServiceScheduling) -> ServiceScheduling
        + Send
        + Sync
        + 'static,
    ) -> Self {
        Self {
            override_resolver: Some(Arc::new(resolver)),
        }
    }
}

#[derive(Clone)]
pub(super) enum BodyExecutionLane {
    Standard(Handle),
    #[cfg(feature = "high-priority")]
    HighPriority {
        shard_id: HighPriorityShardId,
        runtime: Handle,
        decision: HighPriorityPlacementDecision,
        accounting: Arc<HighPriorityRuntimePoolState>,
    },
    Isolated,
    #[cfg(feature = "high-priority")]
    UnavailableHighPriority(HighPriorityPlacementDecision),
}

impl fmt::Debug for BodyExecutionLane {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Standard(_) => formatter.write_str("Standard"),
            #[cfg(feature = "high-priority")]
            Self::HighPriority { shard_id, .. } => {
                write!(formatter, "HighPriority({shard_id})")
            }
            Self::Isolated => formatter.write_str("Isolated"),
            #[cfg(feature = "high-priority")]
            Self::UnavailableHighPriority(_) => formatter.write_str("HighPriority(Unavailable)"),
        }
    }
}

pub(super) struct SpawnServiceParts {
    pub service_instance_id: ServiceInstanceId,
    pub name: &'static str,
    pub run: ServiceFn,
    pub invocation_context: ServiceInvocationContext,
    pub watcher: Option<fn() -> ProviderDependencyWatchSet>,
    pub policy: RestartPolicy,
    pub scheduling: ServiceScheduling,
    pub supervisor_lane: SupervisorSpawnLane,
    pub body_lanes: BodyExecutionLanes,
    pub body_lane_resolver: BodyLaneResolver,
    pub running_tasks: Arc<Mutex<HashMap<ServiceInstanceId, JoinHandle<()>>>>,
    pub resources: Arc<DaemonResources>,
    pub diagnostics: Arc<DiagnosticsStore>,
    pub isolated_startup_permits: Arc<Semaphore>,
    pub cancellation_token: CancellationToken,
    pub daemon_token: CancellationToken,
}

pub(super) struct SpawnAllServicesParts {
    pub instances: Vec<ServiceInstanceRecord>,
    pub restart_policy: RestartPolicy,
    pub running_tasks: Arc<Mutex<HashMap<ServiceInstanceId, JoinHandle<()>>>>,
    pub resources: Arc<DaemonResources>,
    pub diagnostics: Arc<DiagnosticsStore>,
    pub isolated_startup_permits: Arc<Semaphore>,
    pub control_runtime: Handle,
    pub standard_runtime: Handle,
    #[cfg(feature = "high-priority")]
    pub high_priority_pool: Option<Arc<HighPriorityRuntimePoolState>>,
    pub daemon_token: CancellationToken,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_body_lane_resolver_returns_declared_scheduling() {
        let resolver = BodyLaneResolver::default();
        let service_instance_id = ServiceInstanceId::new(uuid::Uuid::from_u128(9));

        assert_eq!(
            resolver.resolve(service_instance_id, 1, ServiceScheduling::Standard),
            ServiceScheduling::Standard
        );
        #[cfg(feature = "high-priority")]
        assert_eq!(
            resolver.resolve(service_instance_id, 2, ServiceScheduling::HighPriority),
            ServiceScheduling::HighPriority
        );
        assert_eq!(
            resolver.resolve(service_instance_id, 3, ServiceScheduling::Isolated),
            ServiceScheduling::Isolated
        );
    }

    #[test]
    fn test_only_body_lane_resolver_override_is_explicit() {
        let resolver = BodyLaneResolver::with_override(|_, _, _| ServiceScheduling::Isolated);

        assert_eq!(
            resolver.resolve(
                ServiceInstanceId::new(uuid::Uuid::from_u128(10)),
                1,
                ServiceScheduling::Standard
            ),
            ServiceScheduling::Isolated
        );
    }
}
