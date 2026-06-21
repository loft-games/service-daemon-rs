//! Protocol codec boundary for the controller bridge adapter.
//!
//! The concrete wire payloads are protobuf messages in `models::protocol`; this
//! module is the adapter-facing boundary that maps connection commands, replies,
//! and events to/from encoded protocol bytes.

use crate::adapter::connection::{DeviceCommand, DeviceEvent, DeviceReply};
use crate::models::protocol::ProtocolEnvelope;

pub struct ProtocolCodec;

impl ProtocolCodec {
    pub fn encode_command(command: &DeviceCommand) -> Vec<u8> {
        ProtocolEnvelope::encode_command(command)
    }

    pub fn encode_reply(reply: &DeviceReply) -> Vec<u8> {
        ProtocolEnvelope::encode_reply(reply)
    }

    pub fn encode_event(event: &DeviceEvent) -> Vec<u8> {
        ProtocolEnvelope::encode_event(event)
    }

    pub fn decode_command(bytes: &[u8]) -> anyhow::Result<Option<DeviceCommand>> {
        ProtocolEnvelope::decode_command(bytes)
    }

    pub fn decode_reply(bytes: &[u8]) -> anyhow::Result<Option<DeviceReply>> {
        ProtocolEnvelope::decode_reply(bytes)
    }

    pub fn decode_event(bytes: &[u8]) -> anyhow::Result<Option<DeviceEvent>> {
        ProtocolEnvelope::decode_event(bytes)
    }
}

#[cfg(test)]
mod tests {
    use crate::adapter::connection::{DeviceCommand, DeviceEvent, DeviceReply};
    use crate::adapter::protocol::ProtocolCodec;

    #[test]
    fn protocol_codec_round_trips_neutral_connection_messages() -> anyhow::Result<()> {
        let command_bytes = ProtocolCodec::encode_command(&DeviceCommand::Ping { sequence: 7 });
        assert_eq!(
            ProtocolCodec::decode_command(&command_bytes)?,
            Some(DeviceCommand::Ping { sequence: 7 })
        );

        let reply_bytes = ProtocolCodec::encode_reply(&DeviceReply::Ack { sequence: 7 });
        assert_eq!(
            ProtocolCodec::decode_reply(&reply_bytes)?,
            Some(DeviceReply::Ack { sequence: 7 })
        );

        let event_bytes = ProtocolCodec::encode_event(&DeviceEvent::Measurement {
            sequence: 8,
            value: 99,
        });
        assert_eq!(
            ProtocolCodec::decode_event(&event_bytes)?,
            Some(DeviceEvent::Measurement {
                sequence: 8,
                value: 99
            })
        );

        let recovered_bytes = ProtocolCodec::encode_event(&DeviceEvent::LinkRecovered {
            sequence: 9,
            reconnects: 2,
        });
        assert_eq!(
            ProtocolCodec::decode_event(&recovered_bytes)?,
            Some(DeviceEvent::LinkRecovered {
                sequence: 9,
                reconnects: 2
            })
        );

        Ok(())
    }
}
