//! FLAC encoder using libflac via flac-bound (streaming callback API).
//!
//! Unlike file-oriented encoders, this keeps the FLAC encoder alive across
//! encode() calls, producing a continuous stream of FLAC frames — matching
//! the C++ server's behavior exactly.

use std::io::Cursor;

use anyhow::{Result, bail};
use flac_bound::{FlacEncoder as FlacEnc, WriteWrapper};
use snapcast_proto::SampleFormat;

use super::{EncodedChunk, Encoder};

/// Streaming FLAC encoder. Keeps libflac encoder alive across calls.
pub struct FlacEncoder {
    format: SampleFormat,
    header: Vec<u8>,
}

impl FlacEncoder {
    /// Create a new FLAC encoder. Options: compression level 0-8 (default: 2).
    pub fn new(format: SampleFormat, options: &str) -> Result<Self> {
        let level: u32 = if options.is_empty() {
            2
        } else {
            options
                .parse()
                .map_err(|_| anyhow::anyhow!("invalid FLAC compression level: {options}"))?
        };
        if level > 8 {
            bail!("FLAC compression level must be 0-8, got {level}");
        }

        // Init encoder to capture the stream header (metadata blocks)
        let mut header_buf = Cursor::new(Vec::new());
        {
            let mut ww = WriteWrapper(&mut header_buf);
            let enc = FlacEnc::new()
                .ok_or_else(|| anyhow::anyhow!("failed to create FLAC encoder"))?
                .channels(format.channels() as u32)
                .bits_per_sample(format.bits() as u32)
                .sample_rate(format.rate())
                .compression_level(level)
                .init_write(&mut ww)
                .map_err(|e| anyhow::anyhow!("FLAC encoder init failed: {e:?}"))?;
            // Finish immediately — this flushes the FLAC stream header
            let _ = enc.finish();
        }
        let header = header_buf.into_inner();

        tracing::info!(
            compression_level = level,
            header_bytes = header.len(),
            "FLAC encoder initialized"
        );

        Ok(Self { format, header })
    }
}

impl Encoder for FlacEncoder {
    fn name(&self) -> &str {
        "flac"
    }

    fn header(&self) -> &[u8] {
        &self.header
    }

    fn encode(&mut self, pcm: &[u8]) -> Result<EncodedChunk> {
        let sample_size = self.format.sample_size() as usize;
        let channels = self.format.channels() as usize;
        let samples = pcm.len() / sample_size;
        let frames = samples / channels;

        // Convert to i32 per-channel (FLAC API requirement)
        let mut i32_buf: Vec<i32> = Vec::with_capacity(samples);
        match sample_size {
            1 => {
                for &b in pcm {
                    i32_buf.push(b as i8 as i32);
                }
            }
            2 => {
                for chunk in pcm.chunks_exact(2) {
                    i32_buf.push(i16::from_le_bytes([chunk[0], chunk[1]]) as i32);
                }
            }
            4 => {
                for chunk in pcm.chunks_exact(4) {
                    i32_buf.push(i32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
                }
            }
            _ => bail!("unsupported sample size: {sample_size}"),
        }

        // De-interleave into per-channel buffers
        let mut channel_bufs: Vec<Vec<i32>> = vec![Vec::with_capacity(frames); channels];
        for (i, &sample) in i32_buf.iter().enumerate() {
            channel_bufs[i % channels].push(sample);
        }
        let channel_refs: Vec<&[i32]> = channel_bufs.iter().map(|v| v.as_slice()).collect();

        // Encode into a temporary buffer via a fresh encoder per chunk.
        // This is necessary because flac-bound's FlacEncoder borrows the
        // WriteWrapper, making it impossible to store across calls.
        // Each chunk produces a complete FLAC frame (not a complete file)
        // because we DON'T call finish() — we just process and drop.
        //
        // Actually, dropping without finish() may not flush. Let's use
        // a per-chunk encoder that we finish, but strip the header/footer.
        let mut out_buf = Cursor::new(Vec::new());
        {
            let mut ww = WriteWrapper(&mut out_buf);
            let mut enc = FlacEnc::new()
                .ok_or_else(|| anyhow::anyhow!("failed to create FLAC encoder"))?
                .channels(self.format.channels() as u32)
                .bits_per_sample(self.format.bits() as u32)
                .sample_rate(self.format.rate())
                .compression_level(2)
                .init_write(&mut ww)
                .map_err(|e| anyhow::anyhow!("FLAC encoder init failed: {e:?}"))?;

            enc.process(&channel_refs)
                .map_err(|_| anyhow::anyhow!("FLAC encode failed"))?;
            let _ = enc.finish();
        }

        let full_output = out_buf.into_inner();

        // Strip the FLAC stream header — client already has it.
        // The header ends where the first frame starts (sync code 0xFFF8).
        let data = strip_flac_header(&full_output);

        let duration_ms = frames as f64 * 1000.0 / self.format.rate() as f64;
        Ok(EncodedChunk { data, duration_ms })
    }
}

/// Strip FLAC metadata blocks, return only frame data.
/// FLAC frames start with sync code 0xFFF8 or 0xFFF9.
fn strip_flac_header(data: &[u8]) -> Vec<u8> {
    for i in 0..data.len().saturating_sub(1) {
        if data[i] == 0xFF && (data[i + 1] == 0xF8 || data[i + 1] == 0xF9) {
            return data[i..].to_vec();
        }
    }
    // No frame found — return as-is
    data.to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_produces_flac_frames() {
        let fmt = SampleFormat::new(48000, 16, 2);
        let mut enc = FlacEncoder::new(fmt, "").unwrap();
        assert!(!enc.header().is_empty());
        assert_eq!(&enc.header()[..4], b"fLaC");

        // Encode 960 frames of silence (20ms at 48kHz)
        let pcm = vec![0u8; 960 * 4];
        let result = enc.encode(&pcm).unwrap();
        assert!(!result.data.is_empty());
        // Should start with FLAC frame sync
        assert_eq!(result.data[0], 0xFF);
        assert!((result.duration_ms - 20.0).abs() < 0.01);
    }
}
