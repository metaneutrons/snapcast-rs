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

/// Create an encoder by codec name.
pub fn create(
    codec: &str,
    format: SampleFormat,
    #[allow(unused_variables)] options: &str,
) -> Result<Box<dyn Encoder>> {
    match codec {
        "pcm" => Ok(Box::new(pcm::PcmEncoder::new(format))),
        #[cfg(feature = "flac")]
        "flac" => Ok(Box::new(flac::FlacEncoder::new(format, options)?)),
        #[cfg(feature = "opus")]
        "opus" => Ok(Box::new(opus::OpusEncoder::new(format, options)?)),
        #[cfg(feature = "vorbis")]
        "ogg" => Ok(Box::new(vorbis::VorbisEncoder::new(format, options)?)),
        #[cfg(feature = "f32lz4")]
        "f32lz4" => Ok(Box::new(f32lz4::F32Lz4Encoder::new(format))),
        other => anyhow::bail!("unsupported codec: {other} (check enabled features)"),
    }
}
