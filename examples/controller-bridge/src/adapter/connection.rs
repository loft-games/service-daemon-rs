//! Controller connection loop and request/response envelope.
//!
//! The connection layer owns transport state, length-prefixed frame assembly,
//! protobuf message decoding, reconnect accounting, and command correlation.
//! Higher layers receive neutral events and never parse bytes directly.

use crate::adapter::codec::FrameCodec;
use crate::adapter::protocol::ProtocolCodec;
use crate::adapter::transport::{InMemoryTransport, TransportRead};
use std::collections::{HashMap, HashSet, VecDeque};

const RETIRED_SEQUENCE_LIMIT: usize = 128;
use std::sync::Arc;
use tokio::sync::{Mutex, oneshot};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceCommand {
    Ping { sequence: u64 },
    Reload { sequence: u64 },
}

impl DeviceCommand {
    pub fn sequence(&self) -> u64 {
        match self {
            Self::Ping { sequence } | Self::Reload { sequence } => *sequence,
        }
    }

    fn accepts_reply(&self, reply: &DeviceReply) -> bool {
        matches!(
            (self, reply),
            (Self::Ping { .. }, DeviceReply::Ack { .. })
                | (Self::Reload { .. }, DeviceReply::Reloaded { .. })
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceReply {
    Ack { sequence: u64 },
    Reloaded { sequence: u64 },
}

impl DeviceReply {
    fn sequence(&self) -> u64 {
        match self {
            Self::Ack { sequence } | Self::Reloaded { sequence } => *sequence,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceEvent {
    Measurement { sequence: u64, value: i32 },
    LinkInterrupted { sequence: u64 },
    LinkRecovered { sequence: u64, reconnects: u64 },
    ReloadRequested { sequence: u64 },
    ProtocolError { sequence: u64, message: String },
    Completed,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ConnectionState {
    #[default]
    Disconnected,
    Connecting,
    Running,
    Closed,
}

impl From<crate::adapter::transport::TransportState> for ConnectionState {
    fn from(state: crate::adapter::transport::TransportState) -> Self {
        match state {
            crate::adapter::transport::TransportState::Disconnected => Self::Disconnected,
            crate::adapter::transport::TransportState::Connected => Self::Running,
            crate::adapter::transport::TransportState::Closed => Self::Closed,
        }
    }
}

#[derive(Debug)]
struct PendingCommand {
    command: DeviceCommand,
    reply: oneshot::Sender<anyhow::Result<DeviceReply>>,
}

#[derive(Debug, Default)]
pub struct DeviceConnection {
    transport: InMemoryTransport,
    codec: FrameCodec,
    reconnects: u64,
    pending_commands: HashMap<u64, PendingCommand>,
    retired_sequences: HashSet<u64>,
    retired_sequences_order: VecDeque<u64>,
    pending_events: VecDeque<DeviceEvent>,
    state: ConnectionState,
}

impl DeviceConnection {
    pub fn new(transport: InMemoryTransport) -> Self {
        Self {
            transport,
            codec: FrameCodec::default(),
            reconnects: 0,
            pending_commands: HashMap::new(),
            retired_sequences: HashSet::new(),
            retired_sequences_order: VecDeque::new(),
            pending_events: VecDeque::new(),
            state: ConnectionState::Disconnected,
        }
    }

    pub fn scripted() -> Self {
        crate::adapter::device::DeviceScript::controller_bridge_demo().into_connection()
    }

    pub fn state(&self) -> ConnectionState {
        self.state.clone()
    }

    pub fn reconnects(&self) -> u64 {
        self.reconnects
    }

    pub fn pending_command_count(&self) -> usize {
        self.pending_commands.len()
    }

    pub fn retired_sequence_count(&self) -> usize {
        self.retired_sequences.len()
    }

    pub async fn connect(&mut self) {
        self.state = ConnectionState::Connecting;
        self.state = self.transport.connect().await.into();
    }

    pub async fn close(&mut self) {
        self.drain_pending_commands("controller connection closed");
        self.transport.close().await;
        self.state = ConnectionState::Closed;
    }

    pub async fn push_inbound_frame(&self, frame: Vec<u8>) {
        self.transport.push_inbound(frame).await;
    }

    pub async fn disconnect_next_recv(&self) {
        self.transport.disconnect_next_recv().await;
    }

    pub async fn outbound_frames(&self) -> Vec<Vec<u8>> {
        self.transport.outbound().await
    }

    pub async fn outbound_payloads(&self) -> anyhow::Result<Vec<Vec<u8>>> {
        let mut codec = FrameCodec::default();
        let mut payloads = Vec::new();
        for frame in self.transport.outbound().await {
            payloads.extend(codec.feed(&frame)?);
        }
        Ok(payloads)
    }

    fn retire_sequence(&mut self, sequence: u64) {
        if self.retired_sequences.len() >= RETIRED_SEQUENCE_LIMIT
            && let Some(oldest) = self.retired_sequences_order.pop_front()
        {
            self.retired_sequences.remove(&oldest);
        }
        if self.retired_sequences.insert(sequence) {
            self.retired_sequences_order.push_back(sequence);
        }
    }

    pub fn cancel_command(&mut self, sequence: u64, reason: impl Into<String>) {
        if let Some(pending) = self.pending_commands.remove(&sequence) {
            self.retire_sequence(sequence);
            let _ = pending.reply.send(Err(anyhow::anyhow!(reason.into())));
        }
    }

    pub async fn send_command(
        &mut self,
        command: DeviceCommand,
    ) -> anyhow::Result<oneshot::Receiver<anyhow::Result<DeviceReply>>> {
        let sequence = command.sequence();
        if self.pending_commands.contains_key(&sequence) {
            anyhow::bail!("command sequence {sequence} is already pending");
        }
        if self.retired_sequences.contains(&sequence) {
            anyhow::bail!("command sequence {sequence} was already used by a retired command");
        }

        let payload = ProtocolCodec::encode_command(&command);
        self.transport.send(FrameCodec::encode(&payload)).await?;

        let (tx, rx) = oneshot::channel();
        self.pending_commands
            .insert(sequence, PendingCommand { command, reply: tx });
        Ok(rx)
    }

    pub async fn recv_event(&mut self) -> anyhow::Result<Option<DeviceEvent>> {
        if let Some(event) = self.pending_events.pop_front() {
            return Ok(Some(event));
        }

        match self.transport.recv().await {
            TransportRead::Bytes(bytes) => {
                let frames = self.codec.feed(&bytes)?;
                for frame in frames {
                    self.handle_frame(frame)?;
                }
                Ok(self.pending_events.pop_front())
            }
            TransportRead::Idle => Ok(None),
            TransportRead::Disconnected => {
                self.state = ConnectionState::Disconnected;
                self.reconnects += 1;
                self.codec.reset();
                self.drain_pending_commands("controller connection disconnected before reply");
                self.connect().await;
                self.pending_events.push_back(DeviceEvent::LinkRecovered {
                    sequence: self.reconnects,
                    reconnects: self.reconnects,
                });
                Ok(Some(DeviceEvent::LinkInterrupted {
                    sequence: self.reconnects,
                }))
            }
        }
    }

    fn handle_frame(&mut self, frame: Vec<u8>) -> anyhow::Result<()> {
        if let Some(reply) = ProtocolCodec::decode_reply(&frame)? {
            let sequence = reply.sequence();
            let Some(pending) = self.pending_commands.remove(&sequence) else {
                tracing::warn!(
                    sequence,
                    ?reply,
                    "ignoring reply with no pending controller command"
                );
                return Ok(());
            };

            self.retire_sequence(sequence);
            if pending.command.accepts_reply(&reply) {
                let _ = pending.reply.send(Ok(reply));
            } else {
                let _ = pending.reply.send(Err(anyhow::anyhow!(
                    "reply {:?} does not match pending command {:?}",
                    reply,
                    pending.command
                )));
            }
            return Ok(());
        }

        if let Some(event) = ProtocolCodec::decode_event(&frame)? {
            self.pending_events.push_back(event);
            return Ok(());
        }

        anyhow::bail!("protocol frame was neither reply nor event")
    }

    fn drain_pending_commands(&mut self, reason: &str) {
        let pending_commands = std::mem::take(&mut self.pending_commands);
        for (sequence, pending) in pending_commands {
            self.retire_sequence(sequence);
            let _ = pending.reply.send(Err(anyhow::anyhow!(reason.to_owned())));
        }
    }

    pub fn event_frame(event: DeviceEvent) -> Vec<u8> {
        FrameCodec::encode(&ProtocolCodec::encode_event(&event))
    }

    pub fn reply_frame(reply: DeviceReply) -> Vec<u8> {
        FrameCodec::encode(&ProtocolCodec::encode_reply(&reply))
    }
}

#[derive(Clone)]
pub struct ConnectionHandle {
    inner: Arc<Mutex<DeviceConnection>>,
}

impl Default for ConnectionHandle {
    fn default() -> Self {
        Self::new(DeviceConnection::scripted())
    }
}

impl ConnectionHandle {
    pub fn new(connection: DeviceConnection) -> Self {
        Self {
            inner: Arc::new(Mutex::new(connection)),
        }
    }

    pub async fn connect(&self) {
        self.inner.lock().await.connect().await;
    }

    pub async fn close(&self) {
        self.inner.lock().await.close().await;
    }

    pub async fn replace_connection(&self, connection: DeviceConnection) {
        *self.inner.lock().await = connection;
    }

    pub async fn recv_event(&self) -> anyhow::Result<Option<DeviceEvent>> {
        self.inner.lock().await.recv_event().await
    }

    pub async fn send_command(
        &self,
        command: DeviceCommand,
    ) -> anyhow::Result<oneshot::Receiver<anyhow::Result<DeviceReply>>> {
        self.inner.lock().await.send_command(command).await
    }

    pub async fn cancel_command(&self, sequence: u64, reason: impl Into<String>) {
        self.inner.lock().await.cancel_command(sequence, reason);
    }

    pub async fn state(&self) -> ConnectionState {
        self.inner.lock().await.state()
    }

    pub async fn reconnects(&self) -> u64 {
        self.inner.lock().await.reconnects()
    }

    pub async fn pending_command_count(&self) -> usize {
        self.inner.lock().await.pending_command_count()
    }

    pub async fn retired_sequence_count(&self) -> usize {
        self.inner.lock().await.retired_sequence_count()
    }

    pub async fn push_inbound_frame(&self, frame: Vec<u8>) {
        self.inner.lock().await.push_inbound_frame(frame).await;
    }

    pub async fn disconnect_next_recv(&self) {
        self.inner.lock().await.disconnect_next_recv().await;
    }

    pub async fn outbound_frames(&self) -> Vec<Vec<u8>> {
        self.inner.lock().await.outbound_frames().await
    }

    pub async fn outbound_payloads(&self) -> anyhow::Result<Vec<Vec<u8>>> {
        self.inner.lock().await.outbound_payloads().await
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ConnectionHandle, ConnectionState, DeviceCommand, DeviceConnection, DeviceEvent,
        DeviceReply,
    };
    use crate::adapter::codec::FrameCodec;

    #[tokio::test]
    async fn connection_decodes_protobuf_frames_and_tracks_reconnects() -> anyhow::Result<()> {
        let handle = ConnectionHandle::new(DeviceConnection::new(Default::default()));
        handle.connect().await;
        handle
            .push_inbound_frame(DeviceConnection::event_frame(DeviceEvent::Measurement {
                sequence: 7,
                value: 99,
            }))
            .await;
        handle.disconnect_next_recv().await;

        assert!(matches!(
            handle.recv_event().await?,
            Some(DeviceEvent::Measurement {
                sequence: 7,
                value: 99
            })
        ));
        assert!(matches!(
            handle.recv_event().await?,
            Some(DeviceEvent::LinkInterrupted { sequence: 1 })
        ));
        assert_eq!(handle.state().await, ConnectionState::Running);
        assert_eq!(handle.reconnects().await, 1);
        Ok(())
    }

    #[tokio::test]
    async fn connection_emits_one_recovery_event_then_preserves_measurements() -> anyhow::Result<()>
    {
        let handle = ConnectionHandle::new(DeviceConnection::new(Default::default()));
        handle.connect().await;
        handle.disconnect_next_recv().await;
        handle
            .push_inbound_frame(DeviceConnection::event_frame(DeviceEvent::Measurement {
                sequence: 3,
                value: 43,
            }))
            .await;
        handle
            .push_inbound_frame(DeviceConnection::event_frame(DeviceEvent::Measurement {
                sequence: 4,
                value: 44,
            }))
            .await;

        assert_eq!(
            handle.recv_event().await?,
            Some(DeviceEvent::LinkInterrupted { sequence: 1 })
        );
        assert_eq!(
            handle.recv_event().await?,
            Some(DeviceEvent::LinkRecovered {
                sequence: 1,
                reconnects: 1
            })
        );
        assert_eq!(
            handle.recv_event().await?,
            Some(DeviceEvent::Measurement {
                sequence: 3,
                value: 43
            })
        );
        assert_eq!(
            handle.recv_event().await?,
            Some(DeviceEvent::Measurement {
                sequence: 4,
                value: 44
            })
        );
        assert_eq!(handle.state().await, ConnectionState::Running);
        Ok(())
    }

    #[tokio::test]
    async fn connection_buffers_split_frames_until_complete() -> anyhow::Result<()> {
        let handle = ConnectionHandle::new(DeviceConnection::new(Default::default()));
        handle.connect().await;
        let frame = DeviceConnection::event_frame(DeviceEvent::Measurement {
            sequence: 9,
            value: 77,
        });
        handle.push_inbound_frame(frame[..3].to_vec()).await;
        handle.push_inbound_frame(frame[3..].to_vec()).await;

        assert!(handle.recv_event().await?.is_none());
        assert_eq!(
            handle.recv_event().await?,
            Some(DeviceEvent::Measurement {
                sequence: 9,
                value: 77
            })
        );
        Ok(())
    }

    #[tokio::test]
    async fn connection_correlates_command_replies_by_sequence() -> anyhow::Result<()> {
        let handle = ConnectionHandle::new(DeviceConnection::new(Default::default()));
        handle.connect().await;
        let reply = handle
            .send_command(DeviceCommand::Ping { sequence: 42 })
            .await?;
        let outbound = handle.outbound_frames().await;
        assert_eq!(outbound.len(), 1);

        handle
            .push_inbound_frame(DeviceConnection::reply_frame(DeviceReply::Ack {
                sequence: 42,
            }))
            .await;
        assert!(handle.recv_event().await?.is_none());
        assert_eq!(reply.await??, DeviceReply::Ack { sequence: 42 });
        Ok(())
    }

    #[tokio::test]
    async fn connection_rejects_mismatched_reply_kind() -> anyhow::Result<()> {
        let handle = ConnectionHandle::new(DeviceConnection::new(Default::default()));
        handle.connect().await;
        let reply = handle
            .send_command(DeviceCommand::Ping { sequence: 42 })
            .await?;

        handle
            .push_inbound_frame(DeviceConnection::reply_frame(DeviceReply::Reloaded {
                sequence: 42,
            }))
            .await;
        assert!(handle.recv_event().await?.is_none());

        let error = reply
            .await?
            .expect_err("reply kind mismatch should be reported");
        assert!(error.to_string().contains("does not match pending command"));
        assert_eq!(handle.pending_command_count().await, 0);
        Ok(())
    }

    #[tokio::test]
    async fn connection_rejects_duplicate_pending_command_sequence() -> anyhow::Result<()> {
        let handle = ConnectionHandle::new(DeviceConnection::new(Default::default()));
        handle.connect().await;
        let _reply = handle
            .send_command(DeviceCommand::Ping { sequence: 42 })
            .await?;

        let error = handle
            .send_command(DeviceCommand::Reload { sequence: 42 })
            .await
            .expect_err("duplicate command sequence should fail");
        assert!(error.to_string().contains("already pending"));
        assert_eq!(handle.pending_command_count().await, 1);
        Ok(())
    }

    #[tokio::test]
    async fn connection_rejects_reused_retired_command_sequence() -> anyhow::Result<()> {
        let handle = ConnectionHandle::new(DeviceConnection::new(Default::default()));
        handle.connect().await;
        let _reply = handle
            .send_command(DeviceCommand::Ping { sequence: 42 })
            .await?;

        handle.cancel_command(42, "test cancellation").await;
        let error = handle
            .send_command(DeviceCommand::Ping { sequence: 42 })
            .await
            .expect_err("retired command sequence should not be reused");
        assert!(error.to_string().contains("retired command"));
        Ok(())
    }

    #[tokio::test]
    async fn connection_rolls_back_or_drains_pending_on_send_failure_and_disconnect()
    -> anyhow::Result<()> {
        let handle = ConnectionHandle::new(DeviceConnection::new(Default::default()));
        let error = handle
            .send_command(DeviceCommand::Ping { sequence: 1 })
            .await
            .expect_err("send should fail while disconnected");
        assert!(error.to_string().contains("transport is not connected"));
        assert_eq!(handle.pending_command_count().await, 0);

        handle.connect().await;
        let reply = handle
            .send_command(DeviceCommand::Ping { sequence: 2 })
            .await?;
        assert_eq!(handle.pending_command_count().await, 1);
        handle.disconnect_next_recv().await;
        assert_eq!(
            handle.recv_event().await?,
            Some(DeviceEvent::LinkInterrupted { sequence: 1 })
        );
        let error = reply
            .await?
            .expect_err("disconnect should complete the pending reply with an error");
        assert!(error.to_string().contains("disconnected before reply"));
        assert_eq!(handle.pending_command_count().await, 0);
        Ok(())
    }

    #[tokio::test]
    async fn connection_ignores_late_orphan_reply_after_disconnect() -> anyhow::Result<()> {
        let handle = ConnectionHandle::new(DeviceConnection::new(Default::default()));
        handle.connect().await;
        let _reply = handle
            .send_command(DeviceCommand::Ping { sequence: 2 })
            .await?;
        handle.disconnect_next_recv().await;
        handle
            .push_inbound_frame(DeviceConnection::reply_frame(DeviceReply::Ack {
                sequence: 2,
            }))
            .await;
        handle
            .push_inbound_frame(DeviceConnection::event_frame(DeviceEvent::Measurement {
                sequence: 3,
                value: 45,
            }))
            .await;

        assert_eq!(
            handle.recv_event().await?,
            Some(DeviceEvent::LinkInterrupted { sequence: 1 })
        );
        assert_eq!(
            handle.recv_event().await?,
            Some(DeviceEvent::LinkRecovered {
                sequence: 1,
                reconnects: 1
            })
        );
        assert!(handle.recv_event().await?.is_none());
        assert_eq!(
            handle.recv_event().await?,
            Some(DeviceEvent::Measurement {
                sequence: 3,
                value: 45
            })
        );
        Ok(())
    }

    #[tokio::test]
    async fn connection_resets_partial_frame_buffer_on_disconnect() -> anyhow::Result<()> {
        let handle = ConnectionHandle::new(DeviceConnection::new(Default::default()));
        handle.connect().await;
        let partial = DeviceConnection::event_frame(DeviceEvent::Measurement {
            sequence: 9,
            value: 77,
        });
        handle.push_inbound_frame(partial[..3].to_vec()).await;
        handle.disconnect_next_recv().await;
        handle
            .push_inbound_frame(DeviceConnection::event_frame(DeviceEvent::Measurement {
                sequence: 10,
                value: 88,
            }))
            .await;

        assert!(handle.recv_event().await?.is_none());
        assert_eq!(
            handle.recv_event().await?,
            Some(DeviceEvent::LinkInterrupted { sequence: 1 })
        );
        assert_eq!(
            handle.recv_event().await?,
            Some(DeviceEvent::LinkRecovered {
                sequence: 1,
                reconnects: 1
            })
        );
        assert_eq!(
            handle.recv_event().await?,
            Some(DeviceEvent::Measurement {
                sequence: 10,
                value: 88
            })
        );
        Ok(())
    }

    #[tokio::test]
    async fn connection_bounds_retired_sequence_tracking() -> anyhow::Result<()> {
        let handle = ConnectionHandle::new(DeviceConnection::new(Default::default()));
        handle.connect().await;

        for sequence in 0..(super::RETIRED_SEQUENCE_LIMIT as u64 + 1) {
            let _reply = handle
                .send_command(DeviceCommand::Ping { sequence })
                .await?;
            handle.cancel_command(sequence, "test cancellation").await;
        }

        assert_eq!(
            handle.retired_sequence_count().await,
            super::RETIRED_SEQUENCE_LIMIT
        );
        handle
            .send_command(DeviceCommand::Ping { sequence: 0 })
            .await?;
        Ok(())
    }

    #[test]
    fn protobuf_payload_still_uses_length_prefix_framing() -> anyhow::Result<()> {
        let mut codec = FrameCodec::default();
        let frame = DeviceConnection::event_frame(DeviceEvent::Completed);
        let frames = codec.feed(&frame)?;
        assert_eq!(frames.len(), 1);
        Ok(())
    }
}
