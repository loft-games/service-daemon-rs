//! Trigger handlers for normalized controller events.
//!
//! Handlers stay thin: the custom host owns connection polling, and the service
//! layer owns side effects such as stats, status updates, and command dispatch.

use crate::adapter::connection::ConnectionHandle;
use crate::models::controller::{ControllerCommandRequest, ControllerEvent, ControllerStatus};
use crate::providers::ControllerCommandQueue;
use crate::services::controller::{
    record_controller_event, record_controller_status_update, send_controller_command,
};
use crate::trigger_templates::ControllerHost;
use service_daemon::TT::*;
use service_daemon::trigger;
use std::sync::Arc;

#[trigger(ControllerHost(ConnectionHandle), scheduling = Isolated)]
pub async fn on_controller_event(#[payload] event: Arc<ControllerEvent>) -> anyhow::Result<()> {
    record_controller_event(event).await
}

#[trigger(Watch(ControllerStatus))]
pub async fn on_controller_status_changed(snapshot: Arc<ControllerStatus>) -> anyhow::Result<()> {
    record_controller_status_update(snapshot).await
}

#[trigger(Queue(ControllerCommandQueue))]
pub async fn on_controller_command(
    #[payload] request: Arc<ControllerCommandRequest>,
    connection: Arc<ConnectionHandle>,
) -> anyhow::Result<()> {
    send_controller_command(request, connection).await
}
