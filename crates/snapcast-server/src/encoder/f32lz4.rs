//! F32 LZ4 encoder — lossless compressed f32 audio.
//!
//! Skips PCM conversion entirely. The wire format is:
//! - Header: "F32L" magic + sample_rate(u32) + channels(u16) + bits(u16) = 12 bytes
//! - Chunks: LZ4-compressed f32 samples (lz4_flex prepend_size format)

use anyhow::Result;
use snapcast_proto::SampleFormat;

use super::{EncodedChunk, Encoder};

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

    fn encode(&mut self, pcm: &[u8]) -> Result<EncodedChunk> {
        let sample_size = self.format.sample_size() as usize;
        let channels = self.format.channels() as usize;

        // If input is already f32 bytes (4 bytes/sample, from audio_tx), compress directly
        // If input is i16 PCM (from pipe readers), convert first
        let f32_bytes = if sample_size == 4 {
            // Already f32 — zero conversion
            pcm.to_vec()
        } else {
            // Convert i16 PCM to f32
            let mut buf = Vec::with_capacity(pcm.len() * 2);
            for chunk in pcm.chunks_exact(2) {
                let s = i16::from_le_bytes([chunk[0], chunk[1]]) as f32 / i16::MAX as f32;
                buf.extend_from_slice(&s.to_le_bytes());
            }
            buf
        };

        let frames = f32_bytes.len() / (4 * channels);
        tracing::trace!(input_bytes = f32_bytes.len(), frames, "F32LZ4 encoding");
        let compressed = lz4_flex::compress_prepend_size(&f32_bytes);
        let duration_ms = self.format.frames_to_ms(frames);

        #[cfg(feature = "encryption")]
        let data = if let Some(ref mut enc) = self.encryptor {
            enc.encrypt(&compressed)
                .map_err(|e| anyhow::anyhow!("encryption failed: {e}"))?
        } else {
            compressed
        };
        #[cfg(not(feature = "encryption"))]
        let data = compressed;

        Ok(EncodedChunk { data, duration_ms })
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
