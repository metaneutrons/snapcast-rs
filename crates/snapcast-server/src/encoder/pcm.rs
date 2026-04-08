//! PCM encoder — passthrough with WAV header.

use anyhow::Result;
use snapcast_proto::SampleFormat;

use super::{EncodedChunk, Encoder};

/// PCM passthrough encoder. Header is a 44-byte WAV header.
pub struct PcmEncoder {
    format: SampleFormat,
    header: Vec<u8>,
}

impl PcmEncoder {
    /// Create a new PCM encoder for the given sample format.
    pub fn new(format: SampleFormat) -> Self {
        let header = build_wav_header(format);
        Self { format, header }
    }
}

impl Encoder for PcmEncoder {
    fn name(&self) -> &str {
        "pcm"
    }

    fn header(&self) -> &[u8] {
        &self.header
    }

    fn encode(&mut self, pcm: &[u8]) -> Result<EncodedChunk> {
        let frame_size = self.format.frame_size() as usize;
        let frames = if frame_size > 0 {
            pcm.len() / frame_size
        } else {
            0
        };
        let duration_ms = frames as f64 * 1000.0 / self.format.rate() as f64;
        Ok(EncodedChunk {
            data: pcm.to_vec(),
            duration_ms,
        })
    }
}

fn build_wav_header(fmt: SampleFormat) -> Vec<u8> {
    let mut h = vec![0u8; 44];
    let channels = fmt.channels();
    let rate = fmt.rate();
    let bits = fmt.bits();
    let block_align = channels * bits.div_ceil(8);
    let byte_rate = rate * block_align as u32;

    h[0..4].copy_from_slice(b"RIFF");
    h[4..8].copy_from_slice(&36u32.to_le_bytes());
    h[8..12].copy_from_slice(b"WAVE");
    h[12..16].copy_from_slice(b"fmt ");
    h[16..20].copy_from_slice(&16u32.to_le_bytes());
    h[20..22].copy_from_slice(&1u16.to_le_bytes()); // PCM
    h[22..24].copy_from_slice(&channels.to_le_bytes());
    h[24..28].copy_from_slice(&rate.to_le_bytes());
    h[28..32].copy_from_slice(&byte_rate.to_le_bytes());
    h[32..34].copy_from_slice(&block_align.to_le_bytes());
    h[34..36].copy_from_slice(&bits.to_le_bytes());
    h[36..40].copy_from_slice(b"data");
    h[40..44].copy_from_slice(&0u32.to_le_bytes());
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pcm_passthrough() {
        let fmt = SampleFormat::new(48000, 16, 2);
        let mut enc = PcmEncoder::new(fmt);
        assert_eq!(enc.name(), "pcm");
        assert_eq!(enc.header().len(), 44);
        assert_eq!(&enc.header()[0..4], b"RIFF");

        let pcm = vec![0u8; 960 * 4]; // 960 frames, 4 bytes/frame
        let result = enc.encode(&pcm).unwrap();
        assert_eq!(result.data.len(), pcm.len());
        assert!((result.duration_ms - 20.0).abs() < 0.01);
    }
}
