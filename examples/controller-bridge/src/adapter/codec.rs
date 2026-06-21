//! Framing codec for the controller bridge protocol.

use anyhow::Context;

const HEADER_LEN: usize = 4;
pub const MAX_FRAME_LEN: usize = 64 * 1024;

#[derive(Debug, Default, Clone)]
pub struct FrameCodec {
    buffer: Vec<u8>,
}

impl FrameCodec {
    pub fn feed(&mut self, bytes: &[u8]) -> anyhow::Result<Vec<Vec<u8>>> {
        self.buffer.extend_from_slice(bytes);
        let mut frames = Vec::new();

        loop {
            if self.buffer.len() < HEADER_LEN {
                return Ok(frames);
            }

            let len = u32::from_be_bytes(
                self.buffer[..HEADER_LEN]
                    .try_into()
                    .context("frame header must be four bytes")?,
            ) as usize;

            if len > MAX_FRAME_LEN {
                self.reset();
                anyhow::bail!("frame length {len} exceeds maximum {MAX_FRAME_LEN}");
            }

            if self.buffer.len() < HEADER_LEN + len {
                return Ok(frames);
            }

            let frame = self.buffer[HEADER_LEN..HEADER_LEN + len].to_vec();
            self.buffer.drain(..HEADER_LEN + len);
            frames.push(frame);
        }
    }

    pub fn reset(&mut self) {
        self.buffer.clear();
    }

    pub fn encode(frame: &[u8]) -> Vec<u8> {
        let mut bytes = (frame.len() as u32).to_be_bytes().to_vec();
        bytes.extend_from_slice(frame);
        bytes
    }
}

#[cfg(test)]
mod tests {
    use crate::adapter::codec::{FrameCodec, MAX_FRAME_LEN};

    #[test]
    fn codec_buffers_partial_frames_until_complete() -> anyhow::Result<()> {
        let mut codec = FrameCodec::default();
        let first = codec.feed(&FrameCodec::encode(b"hello")[..3])?;
        assert!(first.is_empty());

        let second = codec.feed(&FrameCodec::encode(b"hello")[3..])?;
        assert_eq!(second, vec![b"hello".to_vec()]);
        Ok(())
    }

    #[test]
    fn codec_encodes_and_decodes_multiple_frames() -> anyhow::Result<()> {
        let mut codec = FrameCodec::default();
        let mut bytes = FrameCodec::encode(b"one");
        bytes.extend_from_slice(&FrameCodec::encode(b"two"));

        let frames = codec.feed(&bytes)?;
        assert_eq!(frames, vec![b"one".to_vec(), b"two".to_vec()]);
        Ok(())
    }

    #[test]
    fn codec_rejects_oversized_frame_and_can_resynchronize() -> anyhow::Result<()> {
        let mut codec = FrameCodec::default();
        let oversized = ((MAX_FRAME_LEN + 1) as u32).to_be_bytes();

        let error = codec
            .feed(&oversized)
            .expect_err("oversized frame should fail");
        assert!(error.to_string().contains("exceeds maximum"));

        let frames = codec.feed(&FrameCodec::encode(b"next"))?;
        assert_eq!(frames, vec![b"next".to_vec()]);
        Ok(())
    }
}
