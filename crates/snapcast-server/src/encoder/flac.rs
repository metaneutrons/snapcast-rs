//! FLAC encoder using libflac via flac-bound.

use std::io::Cursor;

use anyhow::{Result, bail};
use flac_bound::{FlacEncoder as FlacEnc, WriteWrapper};
use snapcast_proto::SampleFormat;

use super::{EncodedChunk, Encoder};

/// FLAC encoder wrapping libflac.
pub struct FlacEncoder {
    format: SampleFormat,
    header: Vec<u8>,
    output: Cursor<Vec<u8>>,
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

        // Do a dummy init to capture the stream header
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
            // finish immediately to flush the header
            let _ = enc.finish();
        }
        let header = header_buf.into_inner();

        Ok(Self {
            format,
            header,
            output: Cursor::new(Vec::new()),
        })
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
        let channels = self.format.channels() as u32;
        let samples_per_channel = pcm.len() / sample_size / channels as usize;

        // Convert to per-channel i32 slices (FLAC API: &[&[i32]])
        let mut i32_interleaved = Vec::with_capacity(pcm.len() / sample_size);
        match sample_size {
            1 => {
                for &b in pcm {
                    i32_interleaved.push(b as i8 as i32);
                }
            }
            2 => {
                for chunk in pcm.chunks_exact(2) {
                    i32_interleaved.push(i16::from_le_bytes([chunk[0], chunk[1]]) as i32);
                }
            }
            4 => {
                for chunk in pcm.chunks_exact(4) {
                    i32_interleaved
                        .push(i32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
                }
            }
            _ => bail!("unsupported sample size: {sample_size}"),
        }

        // De-interleave into per-channel buffers
        let ch = channels as usize;
        let mut channel_bufs: Vec<Vec<i32>> = vec![Vec::with_capacity(samples_per_channel); ch];
        for (i, &sample) in i32_interleaved.iter().enumerate() {
            channel_bufs[i % ch].push(sample);
        }
        let channel_refs: Vec<&[i32]> = channel_bufs.iter().map(|v| v.as_slice()).collect();

        // Encode
        self.output.get_mut().clear();
        self.output.set_position(0);
        {
            let mut ww = WriteWrapper(&mut self.output);
            let mut enc = FlacEnc::new()
                .ok_or_else(|| anyhow::anyhow!("failed to create FLAC encoder"))?
                .channels(channels)
                .bits_per_sample(self.format.bits() as u32)
                .sample_rate(self.format.rate())
                .compression_level(2)
                .init_write(&mut ww)
                .map_err(|e| anyhow::anyhow!("FLAC encoder init failed: {e:?}"))?;

            enc.process(&channel_refs)
                .map_err(|_| anyhow::anyhow!("FLAC encode failed"))?;
            let _ = enc.finish();
        }

        let duration_ms = samples_per_channel as f64 * 1000.0 / self.format.rate() as f64;
        Ok(EncodedChunk {
            data: self.output.get_ref().clone(),
            duration_ms,
        })
    }
}
