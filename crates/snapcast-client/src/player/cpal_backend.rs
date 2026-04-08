//! Cross-platform audio player using cpal.
//!
//! Works on macOS (CoreAudio), Linux (ALSA), Windows (WASAPI), and more.

use std::sync::{Arc, Mutex};

use anyhow::Result;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat as CpalFormat, StreamConfig};
use snapcast_proto::SampleFormat;

use super::{Player, Volume, apply_volume};
use crate::config::PcmDevice;
use crate::stream::Stream;
use crate::time_provider::TimeProvider;

/// Wrapper to make cpal::Stream Send (it's only accessed from one thread).
struct SendStream(cpal::Stream);
// SAFETY: cpal::Stream is only used from the thread that created it.
// We never move it across threads — it lives in CpalPlayer which stays put.
#[allow(unsafe_code)]
unsafe impl Send for SendStream {}

/// Cross-platform audio player via cpal.
pub struct CpalPlayer {
    stream: Arc<Mutex<Stream>>,
    time_provider: Arc<Mutex<TimeProvider>>,
    volume: Arc<Mutex<Volume>>,
    sample_format: SampleFormat,
    cpal_stream: Option<SendStream>,
}

impl CpalPlayer {
    /// Create a new cpal player.
    pub fn new(
        stream: Arc<Mutex<Stream>>,
        time_provider: Arc<Mutex<TimeProvider>>,
        sample_format: SampleFormat,
    ) -> Self {
        Self {
            stream,
            time_provider,
            volume: Arc::new(Mutex::new(Volume::default())),
            sample_format,
            cpal_stream: None,
        }
    }

    /// List available output devices.
    pub fn pcm_list() -> Result<Vec<PcmDevice>> {
        let host = cpal::default_host();
        let mut devices = Vec::new();
        for (idx, dev) in host.output_devices()?.enumerate() {
            let name = dev.name().unwrap_or_default();
            devices.push(PcmDevice {
                idx: idx as i32,
                name: name.clone(),
                description: name,
            });
        }
        Ok(devices)
    }
}

impl Player for CpalPlayer {
    fn start(&mut self) -> Result<()> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or_else(|| anyhow::anyhow!("no output device available"))?;

        tracing::info!(device = %device.name().unwrap_or_default(), "cpal: using output device");

        let config = StreamConfig {
            channels: self.sample_format.channels(),
            sample_rate: cpal::SampleRate(self.sample_format.rate()),
            buffer_size: cpal::BufferSize::Default,
        };

        let stream_ref = Arc::clone(&self.stream);
        let time_provider = Arc::clone(&self.time_provider);
        let volume = Arc::clone(&self.volume);
        let format = self.sample_format;
        let frame_size = format.frame_size() as usize;

        let cpal_stream = match format.bits() {
            16 => device.build_output_stream(
                &config,
                move |data: &mut [i16], info: &cpal::OutputCallbackInfo| {
                    let num_frames = data.len() / format.channels() as usize;
                    let byte_len = num_frames * frame_size;
                    let mut pcm_buf = vec![0u8; byte_len];

                    // Use cpal's timestamp to compute actual DAC delay
                    let buffer_dac_usec = info
                        .timestamp()
                        .playback
                        .duration_since(&info.timestamp().callback)
                        .map(|d| d.as_micros() as i64)
                        .unwrap_or(0)
                        + (num_frames as i64 * 1_000_000) / format.rate() as i64;

                    let server_now = {
                        let tp = time_provider.lock().unwrap();
                        crate::connection::now_usec() + tp.diff_to_server_usec()
                    };

                    {
                        let mut s = stream_ref.lock().unwrap();
                        s.get_player_chunk_or_silence(
                            server_now,
                            buffer_dac_usec,
                            &mut pcm_buf,
                            num_frames as u32,
                        );
                    }

                    {
                        let vol = volume.lock().unwrap();
                        apply_volume(&mut pcm_buf, format.sample_size(), &vol);
                    }

                    for (i, sample) in data.iter_mut().enumerate() {
                        let offset = i * 2;
                        if offset + 1 < pcm_buf.len() {
                            *sample = i16::from_le_bytes([pcm_buf[offset], pcm_buf[offset + 1]]);
                        }
                    }
                },
                |err| tracing::error!(error = %err, "cpal stream error"),
                None,
            )?,
            _ => anyhow::bail!("cpal: unsupported bit depth {}", format.bits()),
        };

        cpal_stream.play()?;
        tracing::info!("cpal player started");
        self.cpal_stream = Some(SendStream(cpal_stream));
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        self.cpal_stream = None;
        tracing::info!("cpal player stopped");
        Ok(())
    }

    fn set_volume(&mut self, vol: Volume) {
        *self.volume.lock().unwrap() = vol;
    }

    fn volume(&self) -> Volume {
        *self.volume.lock().unwrap()
    }
}
