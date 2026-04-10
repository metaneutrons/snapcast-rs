//! Vorbis encoder using vorbis_rs (libvorbis bindings).

use std::io::Cursor;
use std::num::{NonZeroU8, NonZeroU32};

use anyhow::{Result, bail};
use snapcast_proto::SampleFormat;
use vorbis_rs::VorbisEncoderBuilder;

use super::{EncodedChunk, Encoder};
use crate::AudioData;

/// Vorbis (Ogg) encoder wrapping libvorbis via vorbis_rs.
pub struct VorbisEncoder {
    format: SampleFormat,
    header: Vec<u8>,
}

impl VorbisEncoder {
    /// Create a new Vorbis encoder. Options: not yet used.
    pub fn new(format: SampleFormat, _options: &str) -> Result<Self> {
        // Build header by creating a temporary encoder and capturing the Ogg header pages
        let mut header_buf = Cursor::new(Vec::new());
        let channels = NonZeroU8::new(format.channels() as u8)
            .ok_or_else(|| anyhow::anyhow!("channels must be > 0"))?;
        let rate = NonZeroU32::new(format.rate())
            .ok_or_else(|| anyhow::anyhow!("sample rate must be > 0"))?;

        let enc = VorbisEncoderBuilder::new(rate, channels, &mut header_buf)?.build()?;
        // Finish immediately to flush header pages
        enc.finish()?;

        let header = header_buf.into_inner();

        Ok(Self { format, header })
    }
}

impl Encoder for VorbisEncoder {
    fn name(&self) -> &str {
        "ogg"
    }

    fn header(&self) -> &[u8] {
        &self.header
    }

    fn encode(&mut self, input: &AudioData) -> Result<EncodedChunk> {
        let channels = self.format.channels() as usize;
        if channels == 0 {
            bail!("channels must be > 0");
        }

        // Build per-channel f32 buffers
        let channel_bufs: Vec<Vec<f32>> = match input {
            AudioData::F32(samples) => {
                // Deinterleave f32 directly — zero conversion
                let frames = samples.len() / channels;
                let mut bufs = vec![Vec::with_capacity(frames); channels];
                for (i, &s) in samples.iter().enumerate() {
                    bufs[i % channels].push(s);
                }
                bufs
            }
            AudioData::Pcm(pcm) => {
                let sample_size = self.format.sample_size() as usize;
                let total_samples = pcm.len() / sample_size;
                let frames = total_samples / channels;
                let mut bufs = vec![Vec::with_capacity(frames); channels];
                let scale = match sample_size {
                    2 => 1.0 / 32768.0,
                    4 => 1.0 / 2_147_483_648.0,
                    _ => bail!("unsupported sample size: {sample_size}"),
                };
                match sample_size {
                    2 => {
                        for (i, chunk) in pcm.chunks_exact(2).enumerate() {
                            let s = i16::from_le_bytes([chunk[0], chunk[1]]) as f32 * scale;
                            bufs[i % channels].push(s);
                        }
                    }
                    4 => {
                        for (i, chunk) in pcm.chunks_exact(4).enumerate() {
                            let s = i32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]])
                                as f32
                                * scale;
                            bufs[i % channels].push(s);
                        }
                    }
                    _ => unreachable!(),
                }
                bufs
            }
        };

        let channel_refs: Vec<&[f32]> = channel_bufs.iter().map(|v| v.as_slice()).collect();

        let mut output = Cursor::new(Vec::new());
        let ch = NonZeroU8::new(channels as u8).ok_or_else(|| anyhow::anyhow!("zero channels"))?;
        let rate = NonZeroU32::new(self.format.rate())
            .ok_or_else(|| anyhow::anyhow!("zero sample rate"))?;
        let mut enc = VorbisEncoderBuilder::new(rate, ch, &mut output)?.build()?;
        enc.encode_audio_block(channel_refs)?;
        enc.finish()?;

        Ok(EncodedChunk {
            data: output.into_inner(),
        })
    }
}
