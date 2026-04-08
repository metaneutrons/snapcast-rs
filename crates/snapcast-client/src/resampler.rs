//! Sample rate conversion using rubato.
//!
//! Activated when the server sample rate differs from the player sample rate.

use anyhow::{Result, bail};
use rubato::{FftFixedIn, Resampler as RubatoResampler};
use snapcast_proto::SampleFormat;

/// Resampler that converts between sample rates.
pub struct Resampler {
    resampler: FftFixedIn<f64>,
    in_format: SampleFormat,
    out_format: SampleFormat,
    channels: usize,
}

impl Resampler {
    /// Create a new resampler. Returns `None` if no resampling is needed.
    pub fn new_if_needed(
        in_format: SampleFormat,
        out_format: SampleFormat,
        chunk_frames: usize,
    ) -> Result<Option<Self>> {
        if out_format.rate() == 0 || out_format.rate() == in_format.rate() {
            return Ok(None);
        }

        let channels = in_format.channels() as usize;
        if channels == 0 {
            bail!("cannot resample 0 channels");
        }

        let resampler = FftFixedIn::new(
            in_format.rate() as usize,
            out_format.rate() as usize,
            chunk_frames,
            2, // sub-chunks
            channels,
        )?;

        Ok(Some(Self {
            resampler,
            in_format,
            out_format,
            channels,
        }))
    }

    /// Resample interleaved i16 LE PCM data in-place.
    pub fn process(&mut self, data: &mut Vec<u8>) -> Result<()> {
        let sample_size = self.in_format.sample_size() as usize;
        let frame_size = self.in_format.frame_size() as usize;
        let in_frames = data.len() / frame_size;

        // Deinterleave to f64 channels
        let mut channels_in: Vec<Vec<f64>> = vec![vec![0.0; in_frames]; self.channels];
        for (frame_idx, frame_bytes) in data.chunks_exact(frame_size).enumerate() {
            for (ch, sample_bytes) in frame_bytes.chunks_exact(sample_size).enumerate() {
                let sample = match sample_size {
                    2 => {
                        i16::from_le_bytes([sample_bytes[0], sample_bytes[1]]) as f64
                            / i16::MAX as f64
                    }
                    4 => {
                        i32::from_le_bytes([
                            sample_bytes[0],
                            sample_bytes[1],
                            sample_bytes[2],
                            sample_bytes[3],
                        ]) as f64
                            / i32::MAX as f64
                    }
                    _ => 0.0,
                };
                channels_in[ch][frame_idx] = sample;
            }
        }

        let channels_out = self.resampler.process(&channels_in, None)?;
        let out_frames = channels_out[0].len();
        let out_sample_size = self.out_format.sample_size() as usize;
        let mut out = Vec::with_capacity(out_frames * self.channels * out_sample_size);

        for frame_idx in 0..out_frames {
            for ch_samples in &channels_out {
                let sample = ch_samples[frame_idx];
                match out_sample_size {
                    2 => {
                        let s = (sample * i16::MAX as f64).clamp(i16::MIN as f64, i16::MAX as f64)
                            as i16;
                        out.extend_from_slice(&s.to_le_bytes());
                    }
                    4 => {
                        let s = (sample * i32::MAX as f64).clamp(i32::MIN as f64, i32::MAX as f64)
                            as i32;
                        out.extend_from_slice(&s.to_le_bytes());
                    }
                    _ => out.extend_from_slice(&[0; 2]),
                }
            }
        }

        *data = out;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_resampler_when_same_rate() {
        let fmt = SampleFormat::new(48000, 16, 2);
        let r = Resampler::new_if_needed(fmt, fmt, 480).unwrap();
        assert!(r.is_none());
    }

    #[test]
    fn no_resampler_when_out_rate_zero() {
        let in_fmt = SampleFormat::new(48000, 16, 2);
        let out_fmt = SampleFormat::new(0, 16, 2);
        let r = Resampler::new_if_needed(in_fmt, out_fmt, 480).unwrap();
        assert!(r.is_none());
    }

    #[test]
    fn creates_resampler_for_different_rates() {
        let in_fmt = SampleFormat::new(44100, 16, 2);
        let out_fmt = SampleFormat::new(48000, 16, 2);
        let r = Resampler::new_if_needed(in_fmt, out_fmt, 441).unwrap();
        assert!(r.is_some());
    }

    #[test]
    fn resample_changes_length() {
        let in_fmt = SampleFormat::new(44100, 16, 2);
        let out_fmt = SampleFormat::new(48000, 16, 2);
        let frames = 441; // 10ms at 44100
        let mut r = Resampler::new_if_needed(in_fmt, out_fmt, frames)
            .unwrap()
            .unwrap();

        let in_bytes = frames * in_fmt.frame_size() as usize;
        let mut data = vec![0u8; in_bytes];
        // Fill with a simple pattern
        for (i, chunk) in data.chunks_exact_mut(2).enumerate() {
            let sample = ((i as f64 * 0.1).sin() * 10000.0) as i16;
            chunk.copy_from_slice(&sample.to_le_bytes());
        }

        r.process(&mut data).unwrap();

        // Output is non-empty and differs from input length
        // (rubato FFT resampler has latency, first call produces fewer frames)
        assert!(!data.is_empty());
        assert_ne!(data.len(), in_bytes);
    }
}
