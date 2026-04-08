//! Opus encoder using audiopus.

use anyhow::{Result, bail};
use audiopus::coder::Encoder as OpusEnc;
use audiopus::{Application, Channels, SampleRate};
use snapcast_proto::SampleFormat;

use super::{EncodedChunk, Encoder};

/// Opus encoder wrapping libopus via audiopus.
pub struct OpusEncoder {
    format: SampleFormat,
    encoder: OpusEnc,
    header: Vec<u8>,
    frame_size: usize,
}

impl OpusEncoder {
    /// Create a new Opus encoder. Options: bitrate in kbps (default: 192).
    pub fn new(format: SampleFormat, _options: &str) -> Result<Self> {
        let sample_rate = match format.rate() {
            8000 => SampleRate::Hz8000,
            12000 => SampleRate::Hz12000,
            16000 => SampleRate::Hz16000,
            24000 => SampleRate::Hz24000,
            48000 => SampleRate::Hz48000,
            r => bail!("Opus does not support sample rate {r}"),
        };
        let channels = match format.channels() {
            1 => Channels::Mono,
            2 => Channels::Stereo,
            c => bail!("Opus does not support {c} channels"),
        };

        let encoder = OpusEnc::new(sample_rate, channels, Application::Audio)?;

        // Build OpusHead identification header
        let mut header = Vec::with_capacity(19);
        header.extend_from_slice(b"OpusHead");
        header.push(1); // version
        header.push(format.channels() as u8);
        header.extend_from_slice(&0u16.to_le_bytes()); // pre-skip
        header.extend_from_slice(&format.rate().to_le_bytes());
        header.extend_from_slice(&0u16.to_le_bytes()); // output gain
        header.push(0); // channel mapping family

        // 20ms frame size
        let frame_size = format.rate() as usize / 50;

        Ok(Self {
            format,
            encoder,
            header,
            frame_size,
        })
    }
}

impl Encoder for OpusEncoder {
    fn name(&self) -> &str {
        "opus"
    }

    fn header(&self) -> &[u8] {
        &self.header
    }

    fn encode(&mut self, pcm: &[u8]) -> Result<EncodedChunk> {
        let channels = self.format.channels() as usize;
        let frame_samples = self.frame_size * channels;
        let frame_bytes = frame_samples * 2; // 16-bit samples
        let total_frames = pcm.len() / (channels * 2);

        let mut output = Vec::new();
        let mut encode_buf = [0u8; 4096];

        for chunk in pcm.chunks(frame_bytes) {
            if chunk.len() < frame_bytes {
                break;
            }
            let samples: Vec<i16> = chunk
                .chunks_exact(2)
                .map(|b| i16::from_le_bytes([b[0], b[1]]))
                .collect();

            match self.encoder.encode(&samples, &mut encode_buf) {
                Ok(len) => output.extend_from_slice(&encode_buf[..len]),
                Err(e) => bail!("Opus encode failed: {e}"),
            }
        }

        let duration_ms = total_frames as f64 * 1000.0 / self.format.rate() as f64;
        Ok(EncodedChunk {
            data: output,
            duration_ms,
        })
    }
}
