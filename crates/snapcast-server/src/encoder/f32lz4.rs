//! F32 LZ4 encoder — lossless compressed f32 audio.
//!
//! Skips PCM conversion entirely. The wire format is:
//! - Header: "F32L" magic + sample_rate(u32) + channels(u16) + bits(u16) = 12 bytes
//! - Chunks: LZ4-compressed f32 samples (lz4_flex prepend_size format)

use anyhow::Result;
use snapcast_proto::SampleFormat;

use super::{EncodedChunk, Encoder};
use crate::AudioData;

const MAGIC: &[u8; 4] = b"F32L";

/// Fill buffer with random bytes (uses system RNG).
#[cfg(feature = "encryption")]
fn getrandom(buf: &mut [u8]) {
    use std::io::Read;
    std::fs::File::open("/dev/urandom")
        .expect("open /dev/urandom")
        .read_exact(buf)
        .expect("read /dev/urandom");
}

/// F32 LZ4 encoder — compresses f32 audio with LZ4.
pub struct F32Lz4Encoder {
    format: SampleFormat,
    header: Vec<u8>,
    #[cfg(feature = "encryption")]
    encryptor: Option<crate::crypto::ChunkEncryptor>,
}

impl F32Lz4Encoder {
    /// Create a new F32 LZ4 encoder.
    pub fn new(format: SampleFormat) -> Self {
        tracing::info!(
            rate = format.rate(),
            channels = format.channels(),
            "F32LZ4 encoder initialized"
        );
        let mut header = Vec::with_capacity(12);
        header.extend_from_slice(MAGIC);
        header.extend_from_slice(&format.rate().to_le_bytes());
        header.extend_from_slice(&format.channels().to_le_bytes());
        header.extend_from_slice(&32u16.to_le_bytes()); // bits = 32 (f32)
        Self {
            format,
            header,
            #[cfg(feature = "encryption")]
            encryptor: None,
        }
    }

    /// Enable encryption with a pre-shared key. Appends salt to the codec header.
    #[cfg(feature = "encryption")]
    pub fn with_encryption(mut self, psk: &str) -> Self {
        let mut salt = [0u8; 16];
        getrandom(&mut salt);
        self.encryptor = Some(crate::crypto::ChunkEncryptor::new(psk, &salt));
        // Append encryption marker + salt to header
        self.header.extend_from_slice(b"ENC\0");
        self.header.extend_from_slice(&salt);
        tracing::info!("F32LZ4 encryption enabled");
        self
    }
}

impl Encoder for F32Lz4Encoder {
    fn name(&self) -> &str {
        "f32lz4"
    }

    fn header(&self) -> &[u8] {
        &self.header
    }

    fn encode(&mut self, input: &AudioData) -> Result<EncodedChunk> {
        let channels = self.format.channels() as usize;

        // f32lz4 compresses f32 bytes directly
        let f32_bytes: Vec<u8> = match input {
            AudioData::F32(samples) => {
                // Zero conversion — reinterpret f32 as bytes
                samples.iter().flat_map(|s| s.to_le_bytes()).collect()
            }
            AudioData::Pcm(pcm) => {
                // Convert integer PCM → f32 → bytes
                let f32_samples = super::pcm_to_f32(pcm, self.format.bits());
                f32_samples.iter().flat_map(|s| s.to_le_bytes()).collect()
            }
        };

        let frames = f32_bytes.len() / (4 * channels);
        tracing::trace!(input_bytes = f32_bytes.len(), frames, "F32LZ4 encoding");
        let compressed = lz4_flex::compress_prepend_size(&f32_bytes);

        #[cfg(feature = "encryption")]
        let data = if let Some(ref mut enc) = self.encryptor {
            enc.encrypt(&compressed)
                .map_err(|e| anyhow::anyhow!("encryption failed: {e}"))?
        } else {
            compressed
        };
        #[cfg(not(feature = "encryption"))]
        let data = compressed;

        Ok(EncodedChunk { data })
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
    fn encode_compresses_f32() {
        let fmt = SampleFormat::new(48000, 32, 2);
        let mut enc = F32Lz4Encoder::new(fmt);
        let samples = vec![0.0f32; 960 * 2]; // 960 frames, stereo
        let result = enc.encode(&AudioData::F32(samples)).unwrap();
        assert!(!result.data.is_empty());
    }

    #[test]
    fn encode_compresses_pcm() {
        let fmt = SampleFormat::new(48000, 16, 2);
        let mut enc = F32Lz4Encoder::new(fmt);
        let pcm = vec![0u8; 960 * 4]; // 960 frames, 16-bit stereo
        let result = enc.encode(&AudioData::Pcm(pcm)).unwrap();
        assert!(!result.data.is_empty());
    }
}
