//! FLAC encoder using flac-codec (pure Rust, no C dependencies).

use std::io::Cursor;

use anyhow::Result;
use flac_codec::byteorder::LittleEndian;
use flac_codec::encode::{FlacByteWriter, Options};
use snapcast_proto::SampleFormat;

use super::{EncodedChunk, Encoder};

/// Pure Rust FLAC encoder via flac-codec.
pub struct FlacEncoder {
    format: SampleFormat,
    header: Vec<u8>,
}

impl FlacEncoder {
    /// Create a new FLAC encoder. Options: not yet used.
    pub fn new(format: SampleFormat, _options: &str) -> Result<Self> {
        // Build header by encoding an empty stream
        let mut header_buf = Cursor::new(Vec::new());
        let writer = FlacByteWriter::endian(
            &mut header_buf,
            LittleEndian,
            Options::default(),
            format.rate(),
            format.bits() as u32,
            format.channels() as u8,
            Some(0),
        )?;
        writer.finalize()?;
        let header = header_buf.into_inner();

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
        let frames = pcm.len() / self.format.frame_size() as usize;
        let duration_ms = frames as f64 * 1000.0 / self.format.rate() as f64;
        tracing::trace!(codec = "flac", input_bytes = pcm.len(), frames, "encode");

        let mut output = Cursor::new(Vec::new());
        let mut writer = FlacByteWriter::endian(
            &mut output,
            LittleEndian,
            Options::default(),
            self.format.rate(),
            self.format.bits() as u32,
            self.format.channels() as u8,
            Some(pcm.len() as u64),
        )?;
        std::io::Write::write_all(&mut writer, pcm)?;
        writer.finalize()?;

        Ok(EncodedChunk {
            data: output.into_inner(),
            duration_ms,
        })
    }
}
