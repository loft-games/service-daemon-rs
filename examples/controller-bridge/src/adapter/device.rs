//! Deterministic in-memory device script for the controller bridge example.
//!
//! This is not real hardware. It is a small fixture that feeds the connection
//! layer with ordered bytes and disconnect markers so tests can prove reconnect,
//! framing, trigger, and status behavior without external services.

use crate::adapter::connection::{DeviceConnection, DeviceEvent};
use crate::adapter::transport::InMemoryTransport;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceScriptStep {
    Event(DeviceEvent),
    Disconnect,
}

#[derive(Debug, Clone)]
pub struct DeviceScript {
    steps: Vec<DeviceScriptStep>,
}

impl DeviceScript {
    pub fn controller_bridge_demo() -> Self {
        Self {
            steps: vec![
                DeviceScriptStep::Event(DeviceEvent::Measurement {
                    sequence: 0,
                    value: 40,
                }),
                DeviceScriptStep::Event(DeviceEvent::Measurement {
                    sequence: 1,
                    value: 41,
                }),
                DeviceScriptStep::Disconnect,
                DeviceScriptStep::Event(DeviceEvent::Measurement {
                    sequence: 3,
                    value: 43,
                }),
                DeviceScriptStep::Event(DeviceEvent::ReloadRequested { sequence: 4 }),
                DeviceScriptStep::Event(DeviceEvent::Completed),
            ],
        }
    }

    pub fn into_connection(self) -> DeviceConnection {
        let transport = InMemoryTransport::default();
        for step in self.steps {
            match step {
                DeviceScriptStep::Event(event) => {
                    transport.push_inbound_now(DeviceConnection::event_frame(event));
                }
                DeviceScriptStep::Disconnect => transport.disconnect_next_recv_now(),
            }
        }
        DeviceConnection::new(transport)
    }
}

#[cfg(test)]
mod tests {
    use crate::adapter::connection::DeviceEvent;
    use crate::adapter::device::DeviceScript;

    #[tokio::test]
    async fn device_script_feeds_connection_layer_with_recoverable_sequence() -> anyhow::Result<()>
    {
        let mut connection = DeviceScript::controller_bridge_demo().into_connection();
        connection.connect().await;

        assert_eq!(
            connection.recv_event().await?,
            Some(DeviceEvent::Measurement {
                sequence: 0,
                value: 40
            })
        );
        assert_eq!(
            connection.recv_event().await?,
            Some(DeviceEvent::Measurement {
                sequence: 1,
                value: 41
            })
        );
        assert_eq!(
            connection.recv_event().await?,
            Some(DeviceEvent::LinkInterrupted { sequence: 1 })
        );
        assert_eq!(
            connection.recv_event().await?,
            Some(DeviceEvent::LinkRecovered {
                sequence: 1,
                reconnects: 1
            })
        );

        Ok(())
    }
}
