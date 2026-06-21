//! Neutral protobuf messages used by the controller bridge transport.
//!
//! These messages intentionally model only connection-layer concerns: commands,
//! replies, event notifications, and correlation ids. They avoid source-product
//! fields so the example teaches protocol topology rather than business meaning.

use crate::adapter::connection::{DeviceCommand, DeviceEvent, DeviceReply};
use anyhow::Context;
use prost::Message;

#[derive(Clone, PartialEq, Message)]
pub struct ProtocolEnvelope {
    #[prost(uint64, tag = "1")]
    pub sequence: u64,
    #[prost(oneof = "protocol_envelope::Kind", tags = "2, 3, 4")]
    pub kind: Option<protocol_envelope::Kind>,
}

pub mod protocol_envelope {
    use prost::Oneof;

    #[derive(Clone, PartialEq, Oneof)]
    pub enum Kind {
        #[prost(message, tag = "2")]
        Command(super::CommandMessage),
        #[prost(message, tag = "3")]
        Reply(super::ReplyMessage),
        #[prost(message, tag = "4")]
        Event(super::EventMessage),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, prost::Enumeration)]
#[repr(i32)]
pub enum CommandKind {
    Ping = 0,
    Reload = 1,
}

#[derive(Clone, PartialEq, Message)]
pub struct CommandMessage {
    #[prost(enumeration = "CommandKind", tag = "1")]
    pub kind: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, prost::Enumeration)]
#[repr(i32)]
pub enum ReplyKind {
    Ack = 0,
    Reloaded = 1,
}

#[derive(Clone, PartialEq, Message)]
pub struct ReplyMessage {
    #[prost(enumeration = "ReplyKind", tag = "1")]
    pub kind: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, prost::Enumeration)]
#[repr(i32)]
pub enum EventKind {
    Measurement = 0,
    LinkInterrupted = 1,
    LinkRecovered = 2,
    ReloadRequested = 3,
    Completed = 4,
    ProtocolError = 5,
}

#[derive(Clone, PartialEq, Message)]
pub struct EventMessage {
    #[prost(enumeration = "EventKind", tag = "1")]
    pub kind: i32,
    #[prost(int32, tag = "2")]
    pub value: i32,
    #[prost(uint64, tag = "3")]
    pub reconnects: u64,
    #[prost(string, tag = "4")]
    pub message: String,
}

impl ProtocolEnvelope {
    pub fn encode_command(command: &DeviceCommand) -> Vec<u8> {
        let (sequence, kind) = match command {
            DeviceCommand::Ping { sequence } => (*sequence, CommandKind::Ping),
            DeviceCommand::Reload { sequence } => (*sequence, CommandKind::Reload),
        };

        Self {
            sequence,
            kind: Some(protocol_envelope::Kind::Command(CommandMessage {
                kind: kind as i32,
            })),
        }
        .encode_to_vec()
    }

    pub fn encode_reply(reply: &DeviceReply) -> Vec<u8> {
        let (sequence, kind) = match reply {
            DeviceReply::Ack { sequence } => (*sequence, ReplyKind::Ack),
            DeviceReply::Reloaded { sequence } => (*sequence, ReplyKind::Reloaded),
        };

        Self {
            sequence,
            kind: Some(protocol_envelope::Kind::Reply(ReplyMessage {
                kind: kind as i32,
            })),
        }
        .encode_to_vec()
    }

    pub fn encode_event(event: &DeviceEvent) -> Vec<u8> {
        let (sequence, kind, value, reconnects, message) = match event {
            DeviceEvent::Measurement { sequence, value } => {
                (*sequence, EventKind::Measurement, *value, 0, String::new())
            }
            DeviceEvent::LinkInterrupted { sequence } => {
                (*sequence, EventKind::LinkInterrupted, 0, 0, String::new())
            }
            DeviceEvent::LinkRecovered {
                sequence,
                reconnects,
            } => (
                *sequence,
                EventKind::LinkRecovered,
                0,
                *reconnects,
                String::new(),
            ),
            DeviceEvent::ReloadRequested { sequence } => {
                (*sequence, EventKind::ReloadRequested, 0, 0, String::new())
            }
            DeviceEvent::ProtocolError { sequence, message } => {
                (*sequence, EventKind::ProtocolError, 0, 0, message.clone())
            }
            DeviceEvent::Completed => (0, EventKind::Completed, 0, 0, String::new()),
        };

        Self {
            sequence,
            kind: Some(protocol_envelope::Kind::Event(EventMessage {
                kind: kind as i32,
                value,
                reconnects,
                message,
            })),
        }
        .encode_to_vec()
    }

    pub fn decode_command(bytes: &[u8]) -> anyhow::Result<Option<DeviceCommand>> {
        let envelope =
            Self::decode(bytes).context("protocol command envelope could not be decoded")?;
        let Some(protocol_envelope::Kind::Command(command)) = envelope.kind else {
            return Ok(None);
        };

        let kind = CommandKind::try_from(command.kind).context("unknown command kind")?;
        let command = match kind {
            CommandKind::Ping => DeviceCommand::Ping {
                sequence: envelope.sequence,
            },
            CommandKind::Reload => DeviceCommand::Reload {
                sequence: envelope.sequence,
            },
        };
        Ok(Some(command))
    }

    pub fn decode_reply(bytes: &[u8]) -> anyhow::Result<Option<DeviceReply>> {
        let envelope =
            Self::decode(bytes).context("protocol reply envelope could not be decoded")?;
        let Some(protocol_envelope::Kind::Reply(reply)) = envelope.kind else {
            return Ok(None);
        };

        let kind = ReplyKind::try_from(reply.kind).context("unknown reply kind")?;
        let reply = match kind {
            ReplyKind::Ack => DeviceReply::Ack {
                sequence: envelope.sequence,
            },
            ReplyKind::Reloaded => DeviceReply::Reloaded {
                sequence: envelope.sequence,
            },
        };
        Ok(Some(reply))
    }

    pub fn decode_event(bytes: &[u8]) -> anyhow::Result<Option<DeviceEvent>> {
        let envelope =
            Self::decode(bytes).context("protocol event envelope could not be decoded")?;
        let Some(protocol_envelope::Kind::Event(event)) = envelope.kind else {
            return Ok(None);
        };

        let kind = EventKind::try_from(event.kind).context("unknown event kind")?;
        let decoded = match kind {
            EventKind::Measurement => DeviceEvent::Measurement {
                sequence: envelope.sequence,
                value: event.value,
            },
            EventKind::LinkInterrupted => DeviceEvent::LinkInterrupted {
                sequence: envelope.sequence,
            },
            EventKind::LinkRecovered => DeviceEvent::LinkRecovered {
                sequence: envelope.sequence,
                reconnects: event.reconnects,
            },
            EventKind::ReloadRequested => DeviceEvent::ReloadRequested {
                sequence: envelope.sequence,
            },
            EventKind::ProtocolError => DeviceEvent::ProtocolError {
                sequence: envelope.sequence,
                message: event.message,
            },
            EventKind::Completed => DeviceEvent::Completed,
        };
        Ok(Some(decoded))
    }
}

#[cfg(test)]
mod tests {
    use crate::adapter::connection::{DeviceCommand, DeviceEvent, DeviceReply};
    use crate::models::protocol::ProtocolEnvelope;

    #[test]
    fn protocol_round_trips_command_reply_and_event_payloads() -> anyhow::Result<()> {
        let command_bytes = ProtocolEnvelope::encode_command(&DeviceCommand::Ping { sequence: 7 });
        assert_eq!(
            ProtocolEnvelope::decode_command(&command_bytes)?,
            Some(DeviceCommand::Ping { sequence: 7 })
        );

        let reply_bytes = ProtocolEnvelope::encode_reply(&DeviceReply::Ack { sequence: 7 });
        assert_eq!(
            ProtocolEnvelope::decode_reply(&reply_bytes)?,
            Some(DeviceReply::Ack { sequence: 7 })
        );

        let event_bytes = ProtocolEnvelope::encode_event(&DeviceEvent::Measurement {
            sequence: 8,
            value: 99,
        });
        assert_eq!(
            ProtocolEnvelope::decode_event(&event_bytes)?,
            Some(DeviceEvent::Measurement {
                sequence: 8,
                value: 99
            })
        );

        let interrupted_bytes =
            ProtocolEnvelope::encode_event(&DeviceEvent::LinkInterrupted { sequence: 9 });
        assert_eq!(
            ProtocolEnvelope::decode_event(&interrupted_bytes)?,
            Some(DeviceEvent::LinkInterrupted { sequence: 9 })
        );

        let recovered_bytes = ProtocolEnvelope::encode_event(&DeviceEvent::LinkRecovered {
            sequence: 10,
            reconnects: 2,
        });
        assert_eq!(
            ProtocolEnvelope::decode_event(&recovered_bytes)?,
            Some(DeviceEvent::LinkRecovered {
                sequence: 10,
                reconnects: 2
            })
        );

        Ok(())
    }
}
