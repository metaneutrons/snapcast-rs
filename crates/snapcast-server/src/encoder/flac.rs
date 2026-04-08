//! FLAC encoder using libflac via flac-bound (persistent streaming encoder).

use std::io::{Cursor, Write};

use anyhow::{Result, bail};
use flac_bound::{FlacEncoder as FlacEnc, WriteWrapper};
use snapcast_proto::SampleFormat;

use super::{EncodedChunk, Encoder};

struct FlacEncState {
    _write_wrapper: Box<WriteWrapper<'static>>,
    encoder: FlacEnc<'static>,
}

/// Streaming FLAC encoder that persists across encode() calls.
pub struct FlacEncoder {
    format: SampleFormat,
    header: Vec<u8>,
    encoder: *mut FlacEncState,
    output_buf: *mut Cursor<Vec<u8>>,
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

        // Heap-allocate output buffer (stable address for encoder lifetime)
        let output_buf = Box::into_raw(Box::new(Cursor::new(Vec::with_capacity(8192))));

        // SAFETY: output_buf lives until Drop. Transmute lifetime to 'static.
        let ww: WriteWrapper<'static> = unsafe {
            let buf_ref: &'static mut Cursor<Vec<u8>> = &mut *output_buf;
            std::mem::transmute(WriteWrapper(buf_ref as &mut dyn Write))
        };
        let ww_box = Box::into_raw(Box::new(ww));

        // Init encoder — this writes the FLAC header to output_buf
        let enc = FlacEnc::new()
            .ok_or_else(|| anyhow::anyhow!("failed to create FLAC encoder"))?
            .channels(format.channels() as u32)
            .bits_per_sample(format.bits() as u32)
            .sample_rate(format.rate())
            .compression_level(level);

        let enc = unsafe {
            enc.init_write(&mut *ww_box)
                .map_err(|e| anyhow::anyhow!("FLAC encoder init failed: {e:?}"))?
        };

        // Capture header from THIS encoder instance
        let header = unsafe { (*output_buf).get_ref().clone() };

        // Clear buffer — subsequent process() output goes to encode() return
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
            if !self.encoder.is_null() {
                let state = Box::from_raw(self.encoder);
                let _ = state.encoder.finish();
            }
            if !self.output_buf.is_null() {
                let _ = Box::from_raw(self.output_buf);
            }
        }
    }
}

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
        for i in 0..3 {
            let pcm = vec![((i * 10) & 0xFF) as u8; 960 * 4];
            let result = enc.encode(&pcm).unwrap();
            assert!(
                result.data.is_empty() || result.data[0] == 0xFF,
                "chunk {i}: unexpected data"
            );
        }
    }

    #[test]
    fn encode_produces_output() {
        let fmt = SampleFormat::new(48000, 16, 2);
        let mut enc = FlacEncoder::new(fmt, "").unwrap();
        let mut total = 0;
        for _ in 0..10 {
            let pcm = vec![0u8; 1152 * 4];
            total += enc.encode(&pcm).unwrap().data.len();
        }
        assert!(total > 0, "expected FLAC output after 10 chunks");
    }
}
