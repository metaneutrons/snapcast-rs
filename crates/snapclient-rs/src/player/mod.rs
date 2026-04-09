//! Audio output — cpal callback reads from Stream directly.
//!
//! The cpal hardware callback drives timing. DAC delay is computed from
//! cpal's OutputCallbackInfo timestamps. No ring buffer, no timer drift.

use std::sync::{Arc, Mutex};

use snapcast_client::AudioFrame;
use snapcast_client::connection::now_usec;
use snapcast_client::stream::Stream;
use snapcast_client::time_provider::TimeProvider;
use snapcast_proto::SampleFormat;
use tokio::sync::mpsc;

/// Start audio output. The cpal callback reads from the shared Stream.
/// AudioFrame channel is drained (for future SnapDog use).
pub async fn play_audio(
    mut rx: mpsc::Receiver<AudioFrame>,
    stream: Arc<Mutex<Stream>>,
    time_provider: Arc<Mutex<TimeProvider>>,
    format: SampleFormat,
) {
    // Wait for first audio data to confirm stream is active
    let Some(_first) = rx.recv().await else {
        return;
    };

    // Drain audio_rx in background (not used by this player)
    tokio::spawn(async move { while rx.recv().await.is_some() {} });

    // Start cpal on dedicated thread
    std::thread::spawn(move || {
        if let Err(e) = run_cpal(stream, time_provider, format) {
            tracing::error!(error = %e, "Audio output failed");
        }
    });
}

fn run_cpal(
    stream: Arc<Mutex<Stream>>,
    time_provider: Arc<Mutex<TimeProvider>>,
    format: SampleFormat,
) -> anyhow::Result<()> {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or_else(|| anyhow::anyhow!("no output device"))?;

    tracing::info!(device = %device.name().unwrap_or_default(), "Using audio device");

    let config = cpal::StreamConfig {
        channels: format.channels(),
        sample_rate: cpal::SampleRate(format.rate()),
        buffer_size: cpal::BufferSize::Default,
    };

    let channels = format.channels() as usize;
    let frame_size = format.frame_size() as usize;
    let sample_size = format.sample_size() as usize;

    let cpal_stream = device.build_output_stream(
        &config,
        move |data: &mut [f32], info: &cpal::OutputCallbackInfo| {
            let num_frames = data.len() / channels;
            let mut pcm_buf = vec![0u8; num_frames * frame_size];

            // DAC delay from cpal timestamps (same as old CoreAudio approach)
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

            {
                let mut s = stream.lock().unwrap();
                s.get_player_chunk_or_silence(
                    server_now,
                    buffer_dac_usec,
                    &mut pcm_buf,
                    num_frames as u32,
                );
            }

            // Convert PCM bytes to f32 for cpal
            match sample_size {
                2 => {
                    for (i, chunk) in pcm_buf.chunks_exact(2).enumerate() {
                        data[i] = i16::from_le_bytes([chunk[0], chunk[1]]) as f32 / i16::MAX as f32;
                    }
                }
                4 => {
                    for (i, chunk) in pcm_buf.chunks_exact(4).enumerate() {
                        data[i] = f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                    }
                }
                _ => data.fill(0.0),
            }
        },
        |err| tracing::error!(error = %err, "Audio stream error"),
        None,
    )?;

    cpal_stream.play()?;
    tracing::info!("Audio playback started");

    loop {
        std::thread::sleep(std::time::Duration::from_secs(60));
    }
}
