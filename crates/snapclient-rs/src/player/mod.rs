//! Audio output for the snapclient-rs binary.
//!
//! Uses cpal for cross-platform audio output. The cpal callback pulls
//! time-synced PCM directly from the library's Stream, matching the
//! hardware clock — no timer drift, no ring buffer.

use std::sync::{Arc, Mutex};

use snapcast_client::AudioFrame;
use snapcast_client::connection::now_usec;
use snapcast_client::stream::Stream;
use snapcast_client::time_provider::TimeProvider;
use snapcast_proto::SampleFormat;
use tokio::sync::mpsc;

/// Play audio by pulling from the library's Stream in the cpal callback.
///
/// Waits for the first AudioFrame to learn the format, then creates a cpal
/// output stream that reads directly from the shared Stream + TimeProvider.
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
    tracing::info!(
        sample_rate = format.rate(),
        channels = format.channels(),
        "Audio output starting"
    );

    // Drain rx in background (audio_pump still sends, we just discard)
    tokio::spawn(async move { while rx.recv().await.is_some() {} });

    // Start cpal on a dedicated thread
    let stream_clone = stream;
    let tp_clone = time_provider;
    std::thread::spawn(move || {
        if let Err(e) = run_cpal_output(stream_clone, tp_clone, format) {
            tracing::error!(error = %e, "Audio output failed");
        }
    });
}

fn run_cpal_output(
    stream: Arc<Mutex<Stream>>,
    time_provider: Arc<Mutex<TimeProvider>>,
    format: SampleFormat,
) -> anyhow::Result<()> {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or_else(|| anyhow::anyhow!("no output device"))?;

    tracing::info!(
        device = %device.name().unwrap_or_default(),
        "cpal: using output device"
    );

    let config = cpal::StreamConfig {
        channels: format.channels(),
        sample_rate: cpal::SampleRate(format.rate()),
        buffer_size: cpal::BufferSize::Default,
    };

    let sample_size = format.sample_size() as usize;
    let channels = format.channels() as usize;
    let frame_size = format.frame_size() as usize;

    let cpal_stream = device.build_output_stream(
        &config,
        move |data: &mut [f32], info: &cpal::OutputCallbackInfo| {
            let num_frames = data.len() / channels;
            let byte_len = num_frames * frame_size;
            let mut pcm_buf = vec![0u8; byte_len];

            // Compute DAC delay from cpal timestamps
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

            // Convert PCM bytes to f32
            match sample_size {
                2 => {
                    for (i, chunk) in pcm_buf.chunks_exact(2).enumerate() {
                        let s = i16::from_le_bytes([chunk[0], chunk[1]]);
                        data[i] = s as f32 / i16::MAX as f32;
                    }
                }
                4 => {
                    for (i, chunk) in pcm_buf.chunks_exact(4).enumerate() {
                        let s = i32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                        data[i] = s as f32 / i32::MAX as f32;
                    }
                }
                _ => data.fill(0.0),
            }
        },
        |err| tracing::error!(error = %err, "cpal stream error"),
        None,
    )?;

    cpal_stream.play()?;
    tracing::info!("cpal output started");

    loop {
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}
