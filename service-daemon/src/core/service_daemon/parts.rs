use futures::future::BoxFuture;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::runtime::Handle;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::models::{ServiceFn, ServiceId};

use super::super::context::DaemonResources;
use super::policy::RestartPolicy;

pub(super) struct ServiceSupervisorParts {
    pub service_id: ServiceId,
    pub name: &'static str,
    pub run: ServiceFn,
    pub watcher: Option<fn() -> BoxFuture<'static, ()>>,
    pub policy: RestartPolicy,
    pub resources: Arc<DaemonResources>,
    pub cancellation_token: CancellationToken,
    pub daemon_token: CancellationToken,
}

pub(super) enum SpawnRuntimeLane {
    Standard,
    HighPriority(Handle),
    Isolated,
}

pub(super) struct SpawnServiceParts {
    pub service_id: ServiceId,
    pub name: &'static str,
    pub run: ServiceFn,
    pub watcher: Option<fn() -> BoxFuture<'static, ()>>,
    pub policy: RestartPolicy,
    pub runtime_lane: SpawnRuntimeLane,
    pub running_tasks: Arc<Mutex<HashMap<ServiceId, JoinHandle<()>>>>,
    pub resources: Arc<DaemonResources>,
    pub cancellation_token: CancellationToken,
    pub daemon_token: CancellationToken,
}
