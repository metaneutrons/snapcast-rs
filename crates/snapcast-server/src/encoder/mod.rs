//! Audio encoders — PCM, FLAC, Opus, Vorbis.

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

/// Result of encoding a PCM chunk.
pub struct EncodedChunk {
    /// Encoded audio data.
    pub data: Vec<u8>,
    /// Duration of the encoded audio in milliseconds.
    pub duration_ms: f64,
}

/// Trait for audio encoders.
pub trait Encoder {
    /// Codec name (e.g. "flac", "pcm", "opus", "ogg").
    fn name(&self) -> &str;

    /// Codec header bytes sent to clients before audio data.
    fn header(&self) -> &[u8];

    /// Encode a PCM chunk. Returns encoded data + duration.
    fn encode(&mut self, pcm: &[u8]) -> Result<EncodedChunk>;
}

/// Configuration for creating an encoder.
#[derive(Debug, Clone)]
pub struct EncoderConfig {
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

impl EncoderConfig {
    /// Create a minimal config for the given codec and format.
    pub fn new(codec: &str, format: SampleFormat) -> Self {
        Self {
            codec: codec.into(),
            format,
            options: String::new(),
            #[cfg(feature = "encryption")]
            encryption_psk: None,
        }
    }
}

/// Create an encoder from config.
pub fn create(config: &EncoderConfig) -> Result<Box<dyn Encoder>> {
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
