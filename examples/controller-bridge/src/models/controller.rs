//! Controller event, command, and status models.

use crate::adapter::connection::{ConnectionState, DeviceCommand, DeviceEvent, DeviceReply};
use std::sync::{Arc, Mutex};
use tokio::sync::oneshot;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControllerEvent {
    Measurement { sequence: u64, value: i32 },
    LinkInterrupted { sequence: u64 },
    LinkRecovered { sequence: u64, reconnects: u64 },
    ReloadRequested { sequence: u64 },
    ProtocolError { sequence: u64, message: String },
    Completed,
}

impl From<DeviceEvent> for ControllerEvent {
    fn from(event: DeviceEvent) -> Self {
        match event {
            DeviceEvent::Measurement { sequence, value } => Self::Measurement { sequence, value },
            DeviceEvent::LinkInterrupted { sequence } => Self::LinkInterrupted { sequence },
            DeviceEvent::LinkRecovered {
                sequence,
                reconnects,
            } => Self::LinkRecovered {
                sequence,
                reconnects,
            },
            DeviceEvent::ReloadRequested { sequence } => Self::ReloadRequested { sequence },
            DeviceEvent::ProtocolError { sequence, message } => {
                Self::ProtocolError { sequence, message }
            }
            DeviceEvent::Completed => Self::Completed,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControllerStatus {
    pub state: ConnectionState,
    pub reconnects: u64,
    pub last_event: Option<ControllerEvent>,
    pub updated_count: u64,
}

impl Default for ControllerStatus {
    fn default() -> Self {
        Self {
            state: ConnectionState::Disconnected,
            reconnects: 0,
            last_event: None,
            updated_count: 0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ControllerCommandRequest {
    pub command: DeviceCommand,
    pub reply: ControllerReplyHandle,
}

#[derive(Debug, Clone)]
pub struct ControllerReplyHandle {
    sender: Arc<Mutex<Option<oneshot::Sender<anyhow::Result<DeviceReply>>>>>,
}

impl ControllerReplyHandle {
    pub fn new() -> (Self, oneshot::Receiver<anyhow::Result<DeviceReply>>) {
        let (tx, rx) = oneshot::channel();
        (
            Self {
                sender: Arc::new(Mutex::new(Some(tx))),
            },
            rx,
        )
    }

    pub fn complete(&self, result: anyhow::Result<DeviceReply>) {
        let sender = match self.sender.lock() {
            Ok(mut guard) => guard.take(),
            Err(poisoned) => poisoned.into_inner().take(),
        };
        if let Some(sender) = sender {
            let _ = sender.send(result);
        }
    }
}
