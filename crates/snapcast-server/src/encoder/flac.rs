//! FLAC encoder using libflac-sys directly (raw FFI with streaming callback).
//!
//! Uses the raw C callback API to distinguish header writes from frame writes,
//! matching the C++ server's FlacEncoder exactly.

use std::ffi::c_void;

use anyhow::{Result, bail};
use libflac_sys::*;
use snapcast_proto::SampleFormat;

use super::{EncodedChunk, Encoder};
use crate::AudioData;

/// Client data passed to the libflac write callback.
struct CallbackData {
    header: Vec<u8>,
    frame_buf: Vec<u8>,
    encoded_samples: u32,
}

/// Streaming FLAC encoder using raw libflac FFI.
pub struct FlacEncoder {
    format: SampleFormat,
    encoder: *mut FLAC__StreamEncoder,
    callback_data: *mut CallbackData,
    warned: bool,
}

#[allow(unsafe_code)]
unsafe extern "C" fn write_callback(
    _encoder: *const FLAC__StreamEncoder,
    buffer: *const FLAC__byte,
    bytes: usize,
    samples: u32,
    current_frame: u32,
    client_data: *mut c_void,
) -> FLAC__StreamEncoderWriteStatus {
    let data = unsafe { &mut *(client_data as *mut CallbackData) };
    let slice = unsafe { std::slice::from_raw_parts(buffer, bytes) };

    if current_frame == 0 && samples == 0 {
        // Header/metadata — goes to header buffer
        data.header.extend_from_slice(slice);
    } else {
        // Frame data — goes to frame buffer
        data.frame_buf.extend_from_slice(slice);
        data.encoded_samples += samples;
    }

    0 // FLAC__STREAM_ENCODER_WRITE_STATUS_OK
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

        unsafe {
            let encoder = FLAC__stream_encoder_new();
            if encoder.is_null() {
                bail!("failed to create FLAC encoder");
            }

            FLAC__stream_encoder_set_verify(encoder, 1);
            FLAC__stream_encoder_set_compression_level(encoder, level);
            FLAC__stream_encoder_set_channels(encoder, format.channels() as u32);
            FLAC__stream_encoder_set_bits_per_sample(encoder, format.bits() as u32);
            FLAC__stream_encoder_set_sample_rate(encoder, format.rate());

            let callback_data = Box::into_raw(Box::new(CallbackData {
                header: Vec::new(),
                frame_buf: Vec::new(),
                encoded_samples: 0,
            }));

            let status = FLAC__stream_encoder_init_stream(
                encoder,
                Some(write_callback),
                None, // seek
                None, // tell
                None, // metadata
                callback_data as *mut c_void,
            );

            if status != 0 {
                FLAC__stream_encoder_delete(encoder);
                let _ = Box::from_raw(callback_data);
                bail!("FLAC encoder init failed with status {status}");
            }

            tracing::info!(
                compression_level = level,
                header_bytes = (*callback_data).header.len(),
                "FLAC streaming encoder initialized"
            );

            Ok(Self {
                format,
                encoder,
                callback_data,
                warned: false,
            })
        }
    }
}

#[allow(unsafe_code)]
impl Encoder for FlacEncoder {
    fn name(&self) -> &str {
        "flac"
    }

    fn header(&self) -> &[u8] {
        unsafe { &(*self.callback_data).header }
    }

    fn encode(&mut self, input: &AudioData) -> Result<EncodedChunk> {
        let pcm = match input {
            AudioData::Pcm(data) => std::borrow::Cow::Borrowed(data.as_slice()),
            AudioData::F32(samples) => {
                if !self.warned {
                    self.warned = true;
                    tracing::warn!(
                        codec = "flac",
                        bits = self.format.bits(),
                        "F32 input requires quantization — consider f32lz4 for lossless path"
                    );
                }
                std::borrow::Cow::Owned(super::f32_to_pcm(samples, self.format.bits()))
            }
        };

        let sample_size = self.format.sample_size() as usize;
        let channels = self.format.channels() as usize;
        let samples = pcm.len() / sample_size;
        let frames = samples / channels;

        // Convert to interleaved i32 (libflac process_interleaved format)
        let mut i32_buf: Vec<i32> = Vec::with_capacity(samples);
        match sample_size {
            2 => {
                for chunk in pcm.chunks_exact(2) {
                    i32_buf.push(i16::from_le_bytes([chunk[0], chunk[1]]) as i32);
                }
            }
            4 => {
                for chunk in pcm.chunks_exact(4) {
                    i32_buf.push(i32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
                }
            }
            _ => bail!("unsupported sample size: {sample_size}"),
        }

        unsafe {
            // Clear frame buffer
            (*self.callback_data).frame_buf.clear();
            (*self.callback_data).encoded_samples = 0;

            let ok = FLAC__stream_encoder_process_interleaved(
                self.encoder,
                i32_buf.as_ptr(),
                frames as u32,
            );

            if ok == 0 {
                bail!("FLAC encode failed");
            }

            let data = (*self.callback_data).frame_buf.clone();
            Ok(EncodedChunk { data })
        }
    }
}

#[allow(unsafe_code)]
impl Drop for FlacEncoder {
    fn drop(&mut self) {
        unsafe {
            if !self.encoder.is_null() {
                FLAC__stream_encoder_finish(self.encoder);
                FLAC__stream_encoder_delete(self.encoder);
            }
            if !self.callback_data.is_null() {
                let _ = Box::from_raw(self.callback_data);
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
    fn encode_produces_frames() {
        let fmt = SampleFormat::new(48000, 16, 2);
        let mut enc = FlacEncoder::new(fmt, "").unwrap();
        let mut total = 0;
        for _ in 0..10 {
            let pcm = vec![0u8; 960 * 4]; // 20ms chunks
            let result = enc.encode(&AudioData::Pcm(pcm)).unwrap();
            if !result.data.is_empty() {
                // FLAC frame sync code
                assert_eq!(result.data[0], 0xFF);
                assert!(result.data[1] == 0xF8 || result.data[1] == 0xF9);
            }
            total += result.data.len();
        }
        assert!(total > 0, "expected FLAC output");
    }

    #[test]
    fn persistent_across_chunks() {
        let fmt = SampleFormat::new(48000, 16, 2);
        let mut enc = FlacEncoder::new(fmt, "").unwrap();
        for _ in 0..100 {
            let pcm = vec![42u8; 960 * 4];
            let result = enc.encode(&AudioData::Pcm(pcm)).unwrap();
            if result.data.len() >= 4 {
                assert_ne!(&result.data[..4], b"fLaC", "got header in frame data");
            }
        }
    }
}
