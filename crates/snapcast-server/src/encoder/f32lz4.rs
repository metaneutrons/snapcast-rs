//! F32 LZ4 encoder — lossless compressed f32 audio.
//!
//! Skips PCM conversion entirely. The wire format is:
//! - Header: "F32L" magic + sample_rate(u32) + channels(u16) + bits(u16) = 12 bytes
//! - Chunks: LZ4-compressed f32 samples (lz4_flex prepend_size format)

use anyhow::Result;
use snapcast_proto::SampleFormat;

use super::{EncodedChunk, Encoder};

const MAGIC: &[u8; 4] = b"F32L";

/// F32 LZ4 encoder — compresses f32 audio with LZ4.
pub struct F32Lz4Encoder {
    format: SampleFormat,
    header: Vec<u8>,
}

impl F32Lz4Encoder {
    /// Create a new F32 LZ4 encoder.
    pub fn new(format: SampleFormat) -> Self {
        let mut header = Vec::with_capacity(12);
        header.extend_from_slice(MAGIC);
        header.extend_from_slice(&format.rate().to_le_bytes());
        header.extend_from_slice(&format.channels().to_le_bytes());
        header.extend_from_slice(&32u16.to_le_bytes()); // bits = 32 (f32)
        Self { format, header }
    }
}

impl Encoder for F32Lz4Encoder {
    fn name(&self) -> &str {
        "f32lz4"
    }

    fn header(&self) -> &[u8] {
        &self.header
    }

    fn encode(&mut self, pcm: &[u8]) -> Result<EncodedChunk> {
        let sample_size = self.format.sample_size() as usize;
        let channels = self.format.channels() as usize;
        let frames = pcm.len() / (sample_size * channels);

        // Convert PCM bytes to f32
        let mut f32_bytes = Vec::with_capacity(frames * channels * 4);
        match sample_size {
            2 => {
                for chunk in pcm.chunks_exact(2) {
                    let s = i16::from_le_bytes([chunk[0], chunk[1]]) as f32 / i16::MAX as f32;
                    f32_bytes.extend_from_slice(&s.to_le_bytes());
                }
            }
            4 => {
                // Already 32-bit — just reinterpret as f32
                for chunk in pcm.chunks_exact(4) {
                    let s = i32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]) as f32
                        / i32::MAX as f32;
                    f32_bytes.extend_from_slice(&s.to_le_bytes());
                }
            }
            _ => {
                // Pass through raw bytes
                f32_bytes = pcm.to_vec();
            }
        }

        let compressed = lz4_flex::compress_prepend_size(&f32_bytes);
        let duration_ms = frames as f64 * 1000.0 / self.format.rate() as f64;

        Ok(EncodedChunk {
            data: compressed,
            duration_ms,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_format() {
        let fmt = SampleFormat::new(48000, 16, 2);
        let enc = F32Lz4Encoder::new(fmt);
        assert_eq!(&enc.header()[..4], b"F32L");
        assert_eq!(enc.header().len(), 12);
    }

    #[test]
    fn encode_compresses() {
        let fmt = SampleFormat::new(48000, 16, 2);
        let mut enc = F32Lz4Encoder::new(fmt);
        // 960 * 4 bytes = 960 frames of 16-bit stereo = 20ms at 48kHz
        let pcm = vec![0u8; 960 * 4];
        let result = enc.encode(&pcm).unwrap();
        assert!(result.data.len() < pcm.len(), "expected compression");
        assert!((result.duration_ms - 20.0).abs() < 0.1);
    }
}
