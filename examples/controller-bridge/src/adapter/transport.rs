//! Transport state and testable in-memory byte stream.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TransportState {
    #[default]
    Disconnected,
    Connected,
    Closed,
}

#[derive(Debug)]
enum InboundStep {
    Bytes(Vec<u8>),
    Disconnect,
}

#[derive(Debug, Default)]
struct TransportBuffer {
    inbound: VecDeque<InboundStep>,
    outbound: Vec<Vec<u8>>,
    state: TransportState,
}

#[derive(Debug, Clone, Default)]
pub struct InMemoryTransport {
    buffer: Arc<Mutex<TransportBuffer>>,
}

impl InMemoryTransport {
    fn lock_buffer(&self) -> MutexGuard<'_, TransportBuffer> {
        match self.buffer.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    pub async fn connect(&self) -> TransportState {
        let mut buffer = self.lock_buffer();
        if buffer.state != TransportState::Closed {
            buffer.state = TransportState::Connected;
        }
        buffer.state
    }

    pub async fn close(&self) {
        let mut buffer = self.lock_buffer();
        buffer.state = TransportState::Closed;
    }

    pub async fn state(&self) -> TransportState {
        self.lock_buffer().state
    }

    pub async fn push_inbound(&self, bytes: Vec<u8>) {
        self.push_inbound_now(bytes);
    }

    pub fn push_inbound_now(&self, bytes: Vec<u8>) {
        let mut buffer = self.lock_buffer();
        buffer.inbound.push_back(InboundStep::Bytes(bytes));
    }

    pub async fn disconnect_next_recv(&self) {
        self.disconnect_next_recv_now();
    }

    pub fn disconnect_next_recv_now(&self) {
        let mut buffer = self.lock_buffer();
        buffer.inbound.push_back(InboundStep::Disconnect);
    }

    pub async fn recv(&self) -> TransportRead {
        let mut buffer = self.lock_buffer();
        if buffer.state != TransportState::Connected {
            return TransportRead::Idle;
        }
        match buffer.inbound.pop_front() {
            Some(InboundStep::Bytes(bytes)) => TransportRead::Bytes(bytes),
            Some(InboundStep::Disconnect) => {
                buffer.state = TransportState::Disconnected;
                TransportRead::Disconnected
            }
            None => TransportRead::Idle,
        }
    }

    pub async fn send(&self, bytes: Vec<u8>) -> anyhow::Result<()> {
        let mut buffer = self.lock_buffer();
        if buffer.state != TransportState::Connected {
            anyhow::bail!("transport is not connected")
        }
        buffer.outbound.push(bytes);
        Ok(())
    }

    pub async fn outbound(&self) -> Vec<Vec<u8>> {
        self.lock_buffer().outbound.clone()
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum TransportRead {
    Bytes(Vec<u8>),
    Idle,
    Disconnected,
}

#[cfg(test)]
mod tests {
    use crate::adapter::transport::{InMemoryTransport, TransportRead, TransportState};

    #[tokio::test]
    async fn transport_tracks_connection_state_and_scripted_disconnect() {
        let transport = InMemoryTransport::default();
        assert_eq!(transport.state().await, TransportState::Disconnected);

        transport.connect().await;
        assert_eq!(transport.state().await, TransportState::Connected);

        transport.disconnect_next_recv().await;
        assert_eq!(transport.recv().await, TransportRead::Disconnected);
        assert_eq!(transport.state().await, TransportState::Disconnected);
    }

    #[tokio::test]
    async fn transport_keeps_disconnect_ordered_after_queued_bytes() {
        let transport = InMemoryTransport::default();
        transport.connect().await;
        transport.push_inbound(vec![1, 2]).await;
        transport.disconnect_next_recv().await;

        assert_eq!(transport.recv().await, TransportRead::Bytes(vec![1, 2]));
        assert_eq!(transport.recv().await, TransportRead::Disconnected);
        assert_eq!(transport.state().await, TransportState::Disconnected);
    }

    #[tokio::test]
    async fn transport_does_not_recv_while_disconnected_or_closed() {
        let transport = InMemoryTransport::default();
        transport.push_inbound(vec![1, 2]).await;
        assert_eq!(transport.recv().await, TransportRead::Idle);

        transport.connect().await;
        assert_eq!(transport.recv().await, TransportRead::Bytes(vec![1, 2]));

        transport.push_inbound(vec![3, 4]).await;
        transport.close().await;
        assert_eq!(transport.recv().await, TransportRead::Idle);
        transport.connect().await;
        assert_eq!(transport.state().await, TransportState::Closed);
        assert_eq!(transport.recv().await, TransportRead::Idle);
    }
}
