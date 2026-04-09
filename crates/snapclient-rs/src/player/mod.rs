//! Audio output for the snapclient-rs binary.
//!
//! Reads AudioFrame from the library's audio_rx channel and writes to
//! the audio device. The library's audio_pump is the only Stream reader.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use snapcast_client::AudioFrame;
use tokio::sync::mpsc;

/// Play audio from the library's AudioFrame channel to the default output device.
pub async fn play_audio(mut rx: mpsc::Receiver<AudioFrame>) {
    // Wait for first frame to learn format
    let Some(first) = rx.recv().await else { return };
    tracing::info!(
        sample_rate = first.sample_rate,
        channels = first.channels,
        samples = first.samples.len(),
        "Audio output starting"
    );

    // Ring buffer: pump writes, audio callback reads
    let ring: Arc<Mutex<VecDeque<f32>>> = Arc::new(Mutex::new(VecDeque::with_capacity(
        first.sample_rate as usize,
    )));

    // Seed with first frame
    ring.lock().unwrap().extend(&first.samples);

    // Start audio device on dedicated thread
    let ring_out = Arc::clone(&ring);
    let sample_rate = first.sample_rate;
    let channels = first.channels;

    std::thread::spawn(move || {
        if let Err(e) = run_cpal(ring_out, sample_rate, channels) {
            tracing::error!(error = %e, "Audio device failed");
        }
    });

    // Feed frames into ring buffer from audio_rx
    while let Some(frame) = rx.recv().await {
        let mut buf = ring.lock().unwrap();
        buf.extend(&frame.samples);
        // Cap at 2 seconds to prevent unbounded growth
        let max = sample_rate as usize * channels as usize * 2;
        while buf.len() > max {
            buf.pop_front();
        }
    }
}

fn run_cpal(
    ring: Arc<Mutex<VecDeque<f32>>>,
    sample_rate: u32,
    channels: u16,
) -> anyhow::Result<()> {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or_else(|| anyhow::anyhow!("no output device"))?;

    tracing::info!(device = %device.name().unwrap_or_default(), "Using audio device");

    let config = cpal::StreamConfig {
        channels,
        sample_rate: cpal::SampleRate(sample_rate),
        buffer_size: cpal::BufferSize::Default,
    };

    let stream = device.build_output_stream(
        &config,
        move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
            let mut buf = ring.lock().unwrap();
            for sample in data.iter_mut() {
                *sample = buf.pop_front().unwrap_or(0.0);
            }
        },
        |err| tracing::error!(error = %err, "Audio stream error"),
        None,
    )?;

    stream.play()?;
    tracing::info!("Audio playback started");

    // Keep thread alive
    loop {
        std::thread::sleep(std::time::Duration::from_secs(60));
    }
}
