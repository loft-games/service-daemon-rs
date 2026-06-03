//! Connection provider definitions for the controller bridge example.
//!
//! Providers expose injectable runtime resources. Here the resource is a
//! deterministic in-memory connection handle rather than real hardware.

use crate::adapter::connection::ConnectionHandle;
use crate::models::controller::{ControllerCommandRequest, ControllerStatus};
use crate::services::controller::ControllerEventStatsSnapshot;
use service_daemon::provider;

#[provider]
pub async fn controller_connection_provider() -> ConnectionHandle {
    ConnectionHandle::default()
}

#[provider]
pub async fn controller_status_provider() -> ControllerStatus {
    ControllerStatus::default()
}

#[provider]
pub async fn controller_event_stats_provider() -> ControllerEventStatsSnapshot {
    ControllerEventStatsSnapshot::default()
}

#[provider(Queue(ControllerCommandRequest))]
pub struct ControllerCommandQueue;
