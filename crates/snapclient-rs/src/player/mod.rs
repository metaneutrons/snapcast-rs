//! Audio output for the snapclient-rs binary.
//!
//! Reads AudioFrame from the library's channel and writes to the audio device via cpal.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use snapcast_client::AudioFrame;
use tokio::sync::mpsc;

/// Play audio from the library's AudioFrame channel to the default output device.
pub async fn play_audio(mut rx: mpsc::Receiver<AudioFrame>) {
    let Some(first) = rx.recv().await else { return };
    tracing::info!(
        sample_rate = first.sample_rate,
        channels = first.channels,
        "Audio output starting"
    );

    let ring: Arc<Mutex<VecDeque<f32>>> = Arc::new(Mutex::new(VecDeque::with_capacity(48000)));
    ring.lock().unwrap().extend(&first.samples);

    let ring_clone = Arc::clone(&ring);
    let sample_rate = first.sample_rate;
    let channels = first.channels;

    std::thread::spawn(move || {
        if let Err(e) = run_cpal_output(ring_clone, sample_rate, channels) {
            tracing::error!(error = %e, "Audio output failed");
        }
    });

    while let Some(frame) = rx.recv().await {
        let mut buf = ring.lock().unwrap();
        buf.extend(&frame.samples);
        while buf.len() > 96000 {
            buf.pop_front();
        }
    }
}

fn run_cpal_output(
    ring: Arc<Mutex<VecDeque<f32>>>,
    sample_rate: u32,
    channels: u16,
) -> anyhow::Result<()> {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or_else(|| anyhow::anyhow!("no output device"))?;

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
        |err| tracing::error!(error = %err, "cpal error"),
        None,
    )?;

    stream.play()?;
    tracing::info!("cpal output started");

    loop {
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}
