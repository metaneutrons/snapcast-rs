//! FLAC encoder using libflac via flac-bound (persistent streaming encoder).
//!
//! Uses a self-referential struct to keep the FLAC encoder alive across
//! encode() calls, producing a continuous stream of FLAC frames.

use std::io::{Cursor, Write};

use anyhow::{Result, bail};
use flac_bound::{FlacEncoder as FlacEnc, WriteWrapper};
use snapcast_proto::SampleFormat;

use super::{EncodedChunk, Encoder};

/// Streaming FLAC encoder that persists across encode() calls.
///
/// Uses unsafe to create a self-referential struct: the encoder
/// references the output buffer, both owned by this struct.
pub struct FlacEncoder {
    format: SampleFormat,
    header: Vec<u8>,
    /// Raw pointer to the encoder. We manage its lifetime manually.
    /// The encoder writes to `output_buf` via a raw pointer.
    encoder: *mut FlacEncState,
    /// Output buffer that the encoder writes into.
    output_buf: *mut Cursor<Vec<u8>>,
}

struct FlacEncState {
    /// The WriteWrapper must live as long as the encoder.
    /// We use a Box to get a stable address.
    _write_wrapper: Box<WriteWrapper<'static>>,
    encoder: FlacEnc<'static>,
}

#[allow(unsafe_code)]
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

        // Capture header with a temporary encoder
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
            let _ = enc.finish();
        }
        let header = header_buf.into_inner();

        // Create persistent encoder with stable heap allocations
        let output_buf = Box::into_raw(Box::new(Cursor::new(Vec::with_capacity(8192))));

        // SAFETY: output_buf is heap-allocated and lives until we drop it in Drop.
        // We transmute the lifetime to 'static because we guarantee the buffer
        // outlives the encoder (both are dropped in our Drop impl).
        let ww: WriteWrapper<'static> = unsafe {
            let buf_ref: &'static mut Cursor<Vec<u8>> = &mut *output_buf;
            std::mem::transmute(WriteWrapper(buf_ref as &mut dyn Write))
        };
        let ww_box = Box::into_raw(Box::new(ww));

        let enc = FlacEnc::new()
            .ok_or_else(|| anyhow::anyhow!("failed to create FLAC encoder"))?
            .channels(format.channels() as u32)
            .bits_per_sample(format.bits() as u32)
            .sample_rate(format.rate())
            .compression_level(level);

        // SAFETY: ww_box is heap-allocated and lives until Drop
        let enc = unsafe {
            enc.init_write(&mut *ww_box)
                .map_err(|e| anyhow::anyhow!("FLAC encoder init failed: {e:?}"))?
        };

        // Clear the header that was written during init
        unsafe {
            (*output_buf).get_mut().clear();
            (*output_buf).set_position(0);
        }

        let state = Box::into_raw(Box::new(FlacEncState {
            _write_wrapper: unsafe { Box::from_raw(ww_box) },
            encoder: enc,
        }));

        tracing::info!(
            compression_level = level,
            header_bytes = header.len(),
            "FLAC streaming encoder initialized"
        );

        Ok(Self {
            format,
            header,
            encoder: state,
            output_buf,
        })
    }
}

#[allow(unsafe_code)]
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

        // Convert to per-channel i32 buffers
        let mut channel_bufs: Vec<Vec<i32>> = vec![Vec::with_capacity(frames); channels];
        match sample_size {
            2 => {
                for (i, chunk) in pcm.chunks_exact(2).enumerate() {
                    channel_bufs[i % channels]
                        .push(i16::from_le_bytes([chunk[0], chunk[1]]) as i32);
                }
            }
            4 => {
                for (i, chunk) in pcm.chunks_exact(4).enumerate() {
                    channel_bufs[i % channels]
                        .push(i32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
                }
            }
            _ => bail!("unsupported sample size: {sample_size}"),
        }
        let channel_refs: Vec<&[i32]> = channel_bufs.iter().map(|v| v.as_slice()).collect();

        // Clear output buffer, encode, drain
        // SAFETY: we own output_buf and encoder, and they're valid
        unsafe {
            (*self.output_buf).get_mut().clear();
            (*self.output_buf).set_position(0);

            (*self.encoder)
                .encoder
                .process(&channel_refs)
                .map_err(|_| anyhow::anyhow!("FLAC encode failed"))?;

            let data = (*self.output_buf).get_ref().clone();
            let duration_ms = frames as f64 * 1000.0 / self.format.rate() as f64;
            Ok(EncodedChunk { data, duration_ms })
        }
    }
}

#[allow(unsafe_code)]
impl Drop for FlacEncoder {
    fn drop(&mut self) {
        unsafe {
            // Drop encoder first (it references output_buf)
            if !self.encoder.is_null() {
                let state = Box::from_raw(self.encoder);
                let _ = state.encoder.finish();
                // _write_wrapper dropped here
            }
            // Then drop the buffer
            if !self.output_buf.is_null() {
                let _ = Box::from_raw(self.output_buf);
            }
        }
    }
}

// SAFETY: FlacEncoder is only used on the dedicated encode thread
#[allow(unsafe_code)]
unsafe impl Send for FlacEncoder {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_starts_with_flac() {
        let fmt = SampleFormat::new(48000, 16, 2);
        let enc = FlacEncoder::new(fmt, "").unwrap();
        assert!(!enc.header().is_empty());
        assert_eq!(&enc.header()[..4], b"fLaC");
    }

    #[test]
    fn encode_multiple_chunks() {
        let fmt = SampleFormat::new(48000, 16, 2);
        let mut enc = FlacEncoder::new(fmt, "").unwrap();

        // Encode 3 chunks — persistent encoder should handle all
        for i in 0..3 {
            let pcm = vec![((i * 10) & 0xFF) as u8; 960 * 4];
            let result = enc.encode(&pcm).unwrap();
            // May be empty if libflac hasn't flushed a frame yet
            // (FLAC frames are 1152+ samples at compression level 2)
            assert!(
                result.data.is_empty() || result.data[0] == 0xFF,
                "chunk {i}: unexpected data start: {:02X?}",
                &result.data[..2.min(result.data.len())]
            );
        }
    }

    #[test]
    fn encode_large_chunk_produces_output() {
        let fmt = SampleFormat::new(48000, 16, 2);
        let mut enc = FlacEncoder::new(fmt, "").unwrap();

        // Feed enough data for libflac to produce frames
        // Block size at level 0-2 is 1152, but libflac may buffer more
        let mut total_output = 0;
        for _ in 0..10 {
            let pcm = vec![0u8; 1152 * 4];
            let result = enc.encode(&pcm).unwrap();
            total_output += result.data.len();
        }
        assert!(total_output > 0, "expected FLAC output after 10 chunks");
    }
}
