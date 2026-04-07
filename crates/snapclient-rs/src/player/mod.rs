//! Audio player trait and software volume control.

#[cfg(feature = "alsa")]
pub mod alsa;
#[cfg(feature = "coreaudio")]
pub mod coreaudio;
#[cfg(feature = "pulse")]
pub mod pulse;

use std::sync::Arc;

use anyhow::Result;

use crate::config::PcmDevice;
use crate::stream::Stream;

/// Player volume state.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Volume {
    /// Volume level 0.0..1.0
    pub volume: f64,
    /// Muted?
    pub muted: bool,
}

impl Default for Volume {
    fn default() -> Self {
        Self {
            volume: 1.0,
            muted: false,
        }
    }
}

/// Callback for volume changes (reported back to server).
pub type VolumeCallback = Box<dyn Fn(&Volume) + Send>;

/// Audio player trait.
pub trait Player: Send {
    fn start(&mut self) -> Result<()>;
    fn stop(&mut self) -> Result<()>;
    fn set_volume(&mut self, volume: Volume);
    fn volume(&self) -> Volume;
}

/// Apply software volume to a PCM buffer (16-bit LE samples).
pub fn apply_volume_i16(buffer: &mut [u8], volume: f64, muted: bool) {
    if muted || volume <= 0.0 {
        buffer.fill(0);
        return;
    }
    if (volume - 1.0).abs() < f64::EPSILON {
        return; // no adjustment needed
    }
    for chunk in buffer.chunks_exact_mut(2) {
        let sample = i16::from_le_bytes([chunk[0], chunk[1]]);
        let adjusted = (sample as f64 * volume) as i16;
        let bytes = adjusted.to_le_bytes();
        chunk[0] = bytes[0];
        chunk[1] = bytes[1];
    }
}

/// Apply software volume to a PCM buffer (32-bit LE samples).
pub fn apply_volume_i32(buffer: &mut [u8], volume: f64, muted: bool) {
    if muted || volume <= 0.0 {
        buffer.fill(0);
        return;
    }
    if (volume - 1.0).abs() < f64::EPSILON {
        return;
    }
    for chunk in buffer.chunks_exact_mut(4) {
        let sample = i32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        let adjusted = (sample as f64 * volume) as i32;
        let bytes = adjusted.to_le_bytes();
        chunk[..4].copy_from_slice(&bytes);
    }
}

/// Apply software volume based on sample size.
pub fn apply_volume(buffer: &mut [u8], sample_size: u16, volume: &Volume) {
    match sample_size {
        2 => apply_volume_i16(buffer, volume.volume, volume.muted),
        4 => apply_volume_i32(buffer, volume.volume, volume.muted),
        _ => {}
    }
}

// Suppress unused warning — Stream and PcmDevice will be used by backends
const _: () = {
    fn _assert_send<T: Send>() {}
    fn _check() {
        _assert_send::<Arc<std::sync::Mutex<Stream>>>();
        let _ = PcmDevice::default();
    }
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn volume_default() {
        let v = Volume::default();
        assert!((v.volume - 1.0).abs() < f64::EPSILON);
        assert!(!v.muted);
    }

    #[test]
    fn apply_volume_i16_half() {
        // 16-bit LE sample: 10000
        let mut buf = 10000i16.to_le_bytes().to_vec();
        apply_volume_i16(&mut buf, 0.5, false);
        let result = i16::from_le_bytes([buf[0], buf[1]]);
        assert_eq!(result, 5000);
    }

    #[test]
    fn apply_volume_i16_muted() {
        let mut buf = 10000i16.to_le_bytes().to_vec();
        apply_volume_i16(&mut buf, 0.5, true);
        assert_eq!(buf, [0, 0]);
    }

    #[test]
    fn apply_volume_i16_unity() {
        let original = 12345i16.to_le_bytes().to_vec();
        let mut buf = original.clone();
        apply_volume_i16(&mut buf, 1.0, false);
        assert_eq!(buf, original);
    }

    #[test]
    fn apply_volume_i16_negative_sample() {
        let mut buf = (-20000i16).to_le_bytes().to_vec();
        apply_volume_i16(&mut buf, 0.5, false);
        let result = i16::from_le_bytes([buf[0], buf[1]]);
        assert_eq!(result, -10000);
    }

    #[test]
    fn apply_volume_i32_half() {
        let mut buf = 100000i32.to_le_bytes().to_vec();
        apply_volume_i32(&mut buf, 0.5, false);
        let result = i32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
        assert_eq!(result, 50000);
    }

    #[test]
    fn apply_volume_multi_sample_buffer() {
        // Two 16-bit stereo samples
        let mut buf = Vec::new();
        buf.extend_from_slice(&1000i16.to_le_bytes());
        buf.extend_from_slice(&(-2000i16).to_le_bytes());
        buf.extend_from_slice(&3000i16.to_le_bytes());
        buf.extend_from_slice(&(-4000i16).to_le_bytes());

        apply_volume_i16(&mut buf, 0.25, false);

        let s0 = i16::from_le_bytes([buf[0], buf[1]]);
        let s1 = i16::from_le_bytes([buf[2], buf[3]]);
        let s2 = i16::from_le_bytes([buf[4], buf[5]]);
        let s3 = i16::from_le_bytes([buf[6], buf[7]]);
        assert_eq!(s0, 250);
        assert_eq!(s1, -500);
        assert_eq!(s2, 750);
        assert_eq!(s3, -1000);
    }

    #[test]
    fn apply_volume_dispatch() {
        let mut buf = 10000i16.to_le_bytes().to_vec();
        let vol = Volume {
            volume: 0.5,
            muted: false,
        };
        apply_volume(&mut buf, 2, &vol);
        let result = i16::from_le_bytes([buf[0], buf[1]]);
        assert_eq!(result, 5000);
    }
}
