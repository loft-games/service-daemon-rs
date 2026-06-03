//! Service-side controller event handling.
//!
//! The service layer records observable effects from normalized controller
//! events. It does not decode frames and does not own transport details.

use crate::adapter::connection::{ConnectionHandle, ConnectionState};
use crate::models::controller::{ControllerCommandRequest, ControllerEvent, ControllerStatus};
use std::sync::Arc;
use std::time::Duration;
use tracing::info;

pub const COMMAND_REPLY_TIMEOUT: Duration = Duration::from_secs(2);

struct CommandDispatchGuard {
    connection: Arc<ConnectionHandle>,
    sequence: u64,
    reply: crate::models::controller::ControllerReplyHandle,
    is_active: bool,
}

impl CommandDispatchGuard {
    fn new(
        request: &Arc<ControllerCommandRequest>,
        connection: Arc<ConnectionHandle>,
        sequence: u64,
    ) -> Self {
        Self {
            connection,
            sequence,
            reply: request.reply.clone(),
            is_active: true,
        }
    }

    fn complete(mut self, result: anyhow::Result<crate::adapter::connection::DeviceReply>) {
        self.is_active = false;
        self.reply.complete(result);
    }
}

impl Drop for CommandDispatchGuard {
    fn drop(&mut self) {
        if !self.is_active {
            return;
        }

        self.reply.complete(Err(anyhow::anyhow!(
            "command dispatch dropped before reply"
        )));
        let connection = self.connection.clone();
        let sequence = self.sequence;
        tokio::spawn(async move {
            connection
                .cancel_command(sequence, "command dispatch dropped before reply")
                .await;
        });
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ControllerEventStatsSnapshot {
    pub measurements: u64,
    pub interruptions: u64,
    pub recoveries: u64,
    pub reloads: u64,
    pub protocol_errors: u64,
    pub completions: u64,
    pub status_updates: u64,
    pub last_status: Option<ControllerStatus>,
    pub command_replies: u64,
}

impl ControllerEventStatsSnapshot {
    pub fn record_event(&mut self, event: &ControllerEvent) {
        match event {
            ControllerEvent::Measurement { .. } => self.measurements += 1,
            ControllerEvent::LinkInterrupted { .. } => self.interruptions += 1,
            ControllerEvent::LinkRecovered { .. } => self.recoveries += 1,
            ControllerEvent::ReloadRequested { .. } => self.reloads += 1,
            ControllerEvent::ProtocolError { .. } => self.protocol_errors += 1,
            ControllerEvent::Completed => self.completions += 1,
        }
    }

    pub fn record_status_update(&mut self, snapshot: &ControllerStatus) {
        self.status_updates += 1;
        self.last_status = Some(snapshot.clone());
    }

    pub fn record_command_reply(&mut self) {
        self.command_replies += 1;
    }
}

pub async fn reset_controller_event_stats() {
    let lock = ControllerEventStatsSnapshot::resolve_rwlock().await;
    let mut guard = lock.write().await;
    *guard = ControllerEventStatsSnapshot::default();
}

pub async fn controller_event_stats_snapshot() -> ControllerEventStatsSnapshot {
    ControllerEventStatsSnapshot::resolve()
        .await
        .as_ref()
        .clone()
}

pub async fn record_controller_event(event: Arc<ControllerEvent>) -> anyhow::Result<()> {
    {
        let lock = ControllerEventStatsSnapshot::resolve_rwlock().await;
        let mut guard = lock.write().await;
        guard.record_event(&event);
    }
    publish_controller_status(&event).await;
    info!(?event, "controller event handled");
    Ok(())
}

pub async fn record_controller_status_update(
    snapshot: Arc<ControllerStatus>,
) -> anyhow::Result<()> {
    let lock = ControllerEventStatsSnapshot::resolve_rwlock().await;
    let mut guard = lock.write().await;
    guard.record_status_update(&snapshot);
    info!(?snapshot, "controller status watch handled");
    Ok(())
}

pub async fn send_controller_command(
    request: Arc<ControllerCommandRequest>,
    connection: Arc<ConnectionHandle>,
) -> anyhow::Result<()> {
    send_controller_command_with_timeout(request, connection, COMMAND_REPLY_TIMEOUT).await
}

pub async fn send_controller_command_with_timeout(
    request: Arc<ControllerCommandRequest>,
    connection: Arc<ConnectionHandle>,
    reply_timeout: Duration,
) -> anyhow::Result<()> {
    let sequence = request.command.sequence();
    let reply = match connection.send_command(request.command.clone()).await {
        Ok(reply) => reply,
        Err(error) => {
            request
                .reply
                .complete(Err(anyhow::anyhow!("command could not be sent: {error}")));
            return Ok(());
        }
    };
    let guard = CommandDispatchGuard::new(&request, connection.clone(), sequence);

    let reply_result = tokio::select! {
        _ = service_daemon::wait_shutdown() => {
            connection
                .cancel_command(sequence, "command cancelled before reply")
                .await;
            Err(anyhow::anyhow!("command cancelled before reply"))
        }
        result = tokio::time::timeout(reply_timeout, reply) => {
            match result {
                Ok(Ok(reply)) => reply,
                Ok(Err(error)) => Err(anyhow::anyhow!("command reply channel closed: {error}")),
                Err(_) => {
                    let message = format!("command reply timed out after {reply_timeout:?}");
                    connection.cancel_command(sequence, message.clone()).await;
                    Err(anyhow::anyhow!(message))
                }
            }
        }
    };

    let should_record_reply = reply_result.is_ok();
    guard.complete(reply_result);
    if should_record_reply {
        let lock = ControllerEventStatsSnapshot::resolve_rwlock().await;
        let mut guard = lock.write().await;
        guard.record_command_reply();
    }
    Ok(())
}

async fn publish_controller_status(event: &ControllerEvent) {
    let lock = ControllerStatus::resolve_rwlock().await;
    let mut guard = lock.write().await;
    guard.updated_count += 1;
    guard.last_event = Some(event.clone());
    match event {
        ControllerEvent::LinkInterrupted { .. } => {
            guard.state = ConnectionState::Disconnected;
        }
        ControllerEvent::LinkRecovered { reconnects, .. } => {
            guard.state = ConnectionState::Running;
            guard.reconnects = *reconnects;
        }
        ControllerEvent::Measurement { .. } | ControllerEvent::ReloadRequested { .. } => {
            guard.state = ConnectionState::Running;
        }
        ControllerEvent::ProtocolError { .. } => {
            guard.state = ConnectionState::Running;
        }
        ControllerEvent::Completed => {
            guard.state = ConnectionState::Closed;
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::models::controller::{ControllerEvent, ControllerStatus};
    use crate::services::controller::ControllerEventStatsSnapshot;

    #[test]
    fn controller_event_stats_records_event_categories() {
        let mut state = ControllerEventStatsSnapshot::default();

        state.record_event(&ControllerEvent::Measurement {
            sequence: 0,
            value: 40,
        });
        state.record_event(&ControllerEvent::LinkInterrupted { sequence: 2 });
        state.record_event(&ControllerEvent::LinkRecovered {
            sequence: 3,
            reconnects: 1,
        });
        state.record_event(&ControllerEvent::ReloadRequested { sequence: 4 });
        state.record_event(&ControllerEvent::ProtocolError {
            sequence: 5,
            message: "decode failed".to_string(),
        });
        state.record_event(&ControllerEvent::Completed);
        state.record_status_update(&ControllerStatus::default());
        state.record_command_reply();

        assert_eq!(state.measurements, 1);
        assert_eq!(state.interruptions, 1);
        assert_eq!(state.recoveries, 1);
        assert_eq!(state.reloads, 1);
        assert_eq!(state.protocol_errors, 1);
        assert_eq!(state.completions, 1);
        assert_eq!(state.status_updates, 1);
        assert!(state.last_status.is_some());
        assert_eq!(state.command_replies, 1);
    }
}
