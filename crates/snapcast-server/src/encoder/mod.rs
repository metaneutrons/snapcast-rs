//! Audio encoders — PCM, FLAC, Opus, Vorbis, F32LZ4.

#[cfg(feature = "f32lz4")]
pub mod f32lz4;
#[cfg(feature = "flac")]
pub mod flac;
#[cfg(feature = "opus")]
pub mod opus;
pub mod pcm;
#[cfg(feature = "vorbis")]
pub mod vorbis;

use anyhow::Result;
use snapcast_proto::SampleFormat;

use crate::AudioData;

/// Result of encoding an audio chunk.
pub(crate) struct EncodedChunk {
    /// Encoded audio data.
    pub data: Vec<u8>,
}

/// Trait for audio encoders.
///
/// Each encoder accepts [`AudioData`] (F32 or Pcm) and handles conversion
/// internally. This keeps format-specific logic in the encoder, not the caller.
pub(crate) trait Encoder: Send {
    /// Codec name (e.g. "flac", "pcm", "opus", "ogg", "f32lz4").
    fn name(&self) -> &str;

    /// Codec header bytes sent to clients before audio data.
    fn header(&self) -> &[u8];

    /// Encode an audio chunk. Accepts F32 or Pcm input.
    fn encode(&mut self, input: &AudioData) -> Result<EncodedChunk>;
}

/// Configuration for creating an encoder.
#[derive(Debug, Clone)]
pub(crate) struct EncoderConfig {
    /// Codec name: "pcm", "flac", "opus", "ogg", "f32lz4".
    pub codec: String,
    /// Audio sample format.
    pub format: SampleFormat,
    /// Codec-specific options (e.g. FLAC compression level).
    pub options: String,
    /// Pre-shared key for f32lz4 encryption. `None` = no encryption.
    #[cfg(feature = "encryption")]
    pub encryption_psk: Option<String>,
}

/// Create an encoder from config.
pub(crate) fn create(config: &EncoderConfig) -> Result<Box<dyn Encoder>> {
    #[allow(unused_variables)]
    let EncoderConfig {
        codec,
        format,
        options,
        ..
    } = config;
    let format = *format;
    match codec.as_str() {
        "pcm" => Ok(Box::new(pcm::PcmEncoder::new(format))),
        #[cfg(feature = "flac")]
        "flac" => Ok(Box::new(flac::FlacEncoder::new(format, options)?)),
        #[cfg(feature = "opus")]
        "opus" => Ok(Box::new(opus::OpusEncoder::new(format, options)?)),
        #[cfg(feature = "vorbis")]
        "ogg" => Ok(Box::new(vorbis::VorbisEncoder::new(format, options)?)),
        #[cfg(feature = "f32lz4")]
        "f32lz4" => {
            let enc = f32lz4::F32Lz4Encoder::new(format);
            #[cfg(feature = "encryption")]
            let enc = if let Some(ref key) = config.encryption_psk {
                enc.with_encryption(key)
            } else {
                enc
            };
            Ok(Box::new(enc))
        }
        other => anyhow::bail!("unsupported codec: {other} (check enabled features)"),
    }
}

/// Convert f32 samples to PCM bytes at the given bit depth.
/// Shared helper for encoders that need integer PCM input.
pub(crate) fn f32_to_pcm(samples: &[f32], bits: u16) -> Vec<u8> {
    match bits {
        16 => {
            let mut buf = Vec::with_capacity(samples.len() * 2);
            for &s in samples {
                let i = (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
                buf.extend_from_slice(&i.to_le_bytes());
            }
            buf
        }
        24 => {
            let mut buf = Vec::with_capacity(samples.len() * 3);
            for &s in samples {
                let i = (s.clamp(-1.0, 1.0) * 8_388_607.0) as i32;
                let bytes = i.to_le_bytes();
                buf.extend_from_slice(&bytes[..3]);
            }
            buf
        }
        32 => {
            let mut buf = Vec::with_capacity(samples.len() * 4);
            for &s in samples {
                let i = (s.clamp(-1.0, 1.0) * i32::MAX as f32) as i32;
                buf.extend_from_slice(&i.to_le_bytes());
            }
            buf
        }
        _ => f32_to_pcm(samples, 16),
    }
}

/// Convert PCM bytes to f32 samples at the given bit depth.
/// Shared helper for encoders that need f32 input.
#[cfg(feature = "f32lz4")]
pub(crate) fn pcm_to_f32(pcm: &[u8], bits: u16) -> Vec<f32> {
    match bits {
        16 => pcm
            .chunks_exact(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / i16::MAX as f32)
            .collect(),
        24 => pcm
            .chunks_exact(3)
            .map(|c| {
                let i =
                    i32::from_le_bytes([c[0], c[1], c[2], if c[2] & 0x80 != 0 { 0xFF } else { 0 }]);
                i as f32 / 8_388_607.0
            })
            .collect(),
        32 => pcm
            .chunks_exact(4)
            .map(|c| i32::from_le_bytes([c[0], c[1], c[2], c[3]]) as f32 / i32::MAX as f32)
            .collect(),
        _ => pcm_to_f32(pcm, 16),
    }
}
