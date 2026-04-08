//! Audio encoders — PCM, FLAC, Opus, Vorbis.

pub mod flac;
pub mod opus;
pub mod pcm;
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
pub fn create(codec: &str, format: SampleFormat, options: &str) -> Result<Box<dyn Encoder>> {
    match codec {
        "pcm" => Ok(Box::new(pcm::PcmEncoder::new(format))),
        "flac" => Ok(Box::new(flac::FlacEncoder::new(format, options)?)),
        "opus" => Ok(Box::new(opus::OpusEncoder::new(format, options)?)),
        "ogg" => Ok(Box::new(vorbis::VorbisEncoder::new(format, options)?)),
        other => anyhow::bail!("unsupported codec: {other}"),
    }
}
