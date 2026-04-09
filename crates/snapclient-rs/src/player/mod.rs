//! Audio output for the snapclient-rs binary.
//!
//! Platform-native backends for precise latency control:
//! - macOS: CoreAudio (queries device latency properties)
//! - Linux: ALSA (queries pcm delay)
//! - Windows/other: cpal (cross-platform fallback)

#[cfg(target_os = "linux")]
pub mod alsa;
#[cfg(target_os = "macos")]
pub mod coreaudio;

use std::sync::{Arc, Mutex};

use snapcast_client::AudioFrame;
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
use snapcast_client::connection::now_usec;
use snapcast_client::stream::Stream;
use snapcast_client::time_provider::TimeProvider;
use snapcast_proto::SampleFormat;
use tokio::sync::mpsc;

/// Software volume control.
#[derive(Debug, Clone, Copy)]
pub struct Volume {
    /// Volume level (0.0–1.0).
    pub volume: f64,
    /// Mute state.
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

/// Apply software volume to i16 PCM buffer.
pub fn apply_volume(buffer: &mut [u8], sample_size: u16, volume: &Volume) {
    if sample_size == 2 {
        apply_volume_i16(buffer, volume.volume, volume.muted);
    }
}

fn apply_volume_i16(buffer: &mut [u8], volume: f64, muted: bool) {
    if muted || volume == 0.0 {
        buffer.fill(0);
        return;
    }
    if (volume - 1.0).abs() < f64::EPSILON {
        return;
    }
    for chunk in buffer.chunks_exact_mut(2) {
        let s = i16::from_le_bytes([chunk[0], chunk[1]]);
        let v = (s as f64 * volume) as i16;
        let bytes = v.to_le_bytes();
        chunk[0] = bytes[0];
        chunk[1] = bytes[1];
    }
}

/// Player trait for audio backends.
#[allow(dead_code)]
pub trait Player: Send {
    /// Start playback.
    fn start(&mut self) -> anyhow::Result<()>;
    /// Stop playback.
    fn stop(&mut self) -> anyhow::Result<()>;
    /// Set volume.
    fn set_volume(&mut self, vol: Volume);
    /// Get current volume.
    fn volume(&self) -> Volume;
}

/// Start audio output using the best available backend.
/// Drains audio_rx (for SnapDog consumers) while the native backend
/// reads directly from the shared Stream.
pub async fn play_audio(
    mut rx: mpsc::Receiver<AudioFrame>,
    stream: Arc<Mutex<Stream>>,
    time_provider: Arc<Mutex<TimeProvider>>,
    format: SampleFormat,
) {
    // Wait for first frame to confirm audio is flowing
    let Some(_first) = rx.recv().await else {
        return;
    };

    // Drain rx in background (audio_pump sends for SnapDog, we discard)
    tokio::spawn(async move { while rx.recv().await.is_some() {} });

    // Start native backend
    #[cfg(target_os = "macos")]
    {
        let mut player = coreaudio::CoreAudioPlayer::new(stream, time_provider, format);
        if let Err(e) = player.start() {
            tracing::error!(error = %e, "CoreAudio failed, no audio output");
            return;
        }
        // Keep alive
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    }

    #[cfg(target_os = "linux")]
    {
        let mut player = alsa::AlsaPlayer::new(stream, time_provider, format, "default");
        if let Err(e) = player.start() {
            tracing::error!(error = %e, "ALSA failed, no audio output");
            return;
        }
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        run_cpal_fallback(stream, time_provider, format);
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    }
}

/// cpal fallback for platforms without a native backend.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn run_cpal_fallback(
    stream: Arc<Mutex<Stream>>,
    time_provider: Arc<Mutex<TimeProvider>>,
    format: SampleFormat,
) {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

    let host = cpal::default_host();
    let device = host.default_output_device().expect("no output device");
    let config = cpal::StreamConfig {
        channels: format.channels(),
        sample_rate: cpal::SampleRate(format.rate()),
        buffer_size: cpal::BufferSize::Default,
    };
    let channels = format.channels() as usize;
    let frame_size = format.frame_size() as usize;
    let sample_size = format.sample_size() as usize;

    let cpal_stream = device
        .build_output_stream(
            &config,
            move |data: &mut [f32], info: &cpal::OutputCallbackInfo| {
                let num_frames = data.len() / channels;
                let mut pcm_buf = vec![0u8; num_frames * frame_size];
                let buffer_dac_usec = info
                    .timestamp()
                    .playback
                    .duration_since(&info.timestamp().callback)
                    .map(|d| d.as_micros() as i64)
                    .unwrap_or(0)
                    + (num_frames as i64 * 1_000_000) / format.rate() as i64;
                let server_now = {
                    let tp = time_provider.lock().unwrap();
                    now_usec() + tp.diff_to_server_usec()
                };
                stream.lock().unwrap().get_player_chunk_or_silence(
                    server_now,
                    buffer_dac_usec,
                    &mut pcm_buf,
                    num_frames as u32,
                );
                match sample_size {
                    2 => {
                        for (i, chunk) in pcm_buf.chunks_exact(2).enumerate() {
                            data[i] =
                                i16::from_le_bytes([chunk[0], chunk[1]]) as f32 / i16::MAX as f32;
                        }
                    }
                    _ => data.fill(0.0),
                }
            },
            |err| tracing::error!(error = %err, "cpal error"),
            None,
        )
        .expect("cpal build_output_stream failed");
    cpal_stream.play().expect("cpal play failed");
    tracing::info!("cpal fallback output started");
}
