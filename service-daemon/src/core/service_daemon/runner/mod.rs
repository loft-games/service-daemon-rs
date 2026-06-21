//! Service runner logic for spawning, supervising, and stopping services.
//!
//! The core abstraction is [`ServiceSupervisor`], which manages a single
//! service's lifecycle using an explicit **Finite State Machine (FSM)**.
//! The FSM transitions through the following states:
//!
//! ```text
//!   Starting --> Running --> Outcome --> Restart --> Starting (loop)
//!      |            |           |                        |
//!      +------------+-----------+-- Terminated <---------+
//! ```

mod generation;
mod supervisor;
mod wave;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::core::context::DaemonResources;
use crate::models::{ServiceDescription, ServiceId};

use super::parts::{SpawnAllServicesParts, SpawnServiceParts};

/// Spawn a single service with the given restart policy.
pub async fn spawn_service(parts: SpawnServiceParts) {
    supervisor::spawn_service(parts).await;
}

/// Spawn all registered services using wave-based priorities.
pub async fn spawn_all_services(parts: SpawnAllServicesParts) {
    wave::spawn_all_services(parts).await;
}

/// Stop all running services gracefully using wave-based priorities.
pub async fn stop_all_services(
    services: &[ServiceDescription],
    running_tasks: Arc<Mutex<HashMap<ServiceId, JoinHandle<()>>>>,
    resources: Arc<DaemonResources>,
    daemon_token: CancellationToken,
    grace_period: Duration,
) {
    wave::stop_all_services(
        services,
        running_tasks,
        resources,
        daemon_token,
        grace_period,
    )
    .await;
}
