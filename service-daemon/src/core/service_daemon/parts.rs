use futures::future::BoxFuture;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::runtime::Handle;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::core::diagnostics::DiagnosticsStore;
use crate::models::{ServiceFn, ServiceId, ServiceScheduling};

use super::super::context::DaemonResources;
use super::policy::RestartPolicy;

pub(super) struct ServiceSupervisorParts {
    pub service_id: ServiceId,
    pub name: &'static str,
    pub run: ServiceFn,
    pub watcher: Option<fn() -> BoxFuture<'static, ()>>,
    pub policy: RestartPolicy,
    pub scheduling: ServiceScheduling,
    pub generation_lane: GenerationExecutionLane,
    pub resources: Arc<DaemonResources>,
    pub diagnostics: Arc<DiagnosticsStore>,
    pub cancellation_token: CancellationToken,
    pub daemon_token: CancellationToken,
}

#[derive(Clone)]
pub(super) enum SupervisorSpawnLane {
    Standard,
    HighPriority(Handle),
}

#[derive(Clone, Copy, Debug)]
pub(super) enum GenerationExecutionLane {
    CurrentRuntime,
    Isolated,
}

pub(super) struct SpawnServiceParts {
    pub service_id: ServiceId,
    pub name: &'static str,
    pub run: ServiceFn,
    pub watcher: Option<fn() -> BoxFuture<'static, ()>>,
    pub policy: RestartPolicy,
    pub scheduling: ServiceScheduling,
    pub supervisor_lane: SupervisorSpawnLane,
    pub generation_lane: GenerationExecutionLane,
    pub running_tasks: Arc<Mutex<HashMap<ServiceId, JoinHandle<()>>>>,
    pub resources: Arc<DaemonResources>,
    pub diagnostics: Arc<DiagnosticsStore>,
    pub cancellation_token: CancellationToken,
    pub daemon_token: CancellationToken,
}
