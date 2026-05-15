//! Audio output — cpal callback reads from Stream directly.

use std::sync::{Arc, Mutex};

use snapcast_client::AudioFrame;
use snapcast_client::connection::now_usec;
use snapcast_client::stream::Stream;
use snapcast_client::time_provider::TimeProvider;
use tokio::sync::mpsc;

use crate::mixer::VolumeState;

/// Start audio output. Waits for the Stream to have audio, then starts cpal.
pub async fn play_audio(
    rx: mpsc::Receiver<AudioFrame>,
    stream: Arc<Mutex<Stream>>,
    time_provider: Arc<Mutex<TimeProvider>>,
    volume: Arc<VolumeState>,
) {
    // Drain audio_rx in background
    tokio::spawn(async move {
        let mut rx = rx;
        while rx.recv().await.is_some() {}
    });

    loop {
        // Wait for the Stream to have a valid format
        let format = loop {
            {
                let s = stream.lock().unwrap_or_else(|e| e.into_inner());
                let f = s.format();
                if f.rate() > 0 && f.channels() > 0 {
                    break f;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        };

        tracing::info!(
            rate = format.rate(),
            bits = format.bits(),
            channels = format.channels(),
            "Audio format detected"
        );

        // Run cpal on dedicated thread. If it returns, it's either an error or a format change.
        let stream_clone = Arc::clone(&stream);
        let tp_clone = Arc::clone(&time_provider);
        let vol_clone = Arc::clone(&volume);

        let handle =
            std::thread::spawn(move || run_cpal(stream_clone, tp_clone, format, vol_clone));

        match handle.join() {
            Ok(Ok(_)) => {
                tracing::info!("Audio format change detected, restarting player");
            }
            Ok(Err(e)) => {
                tracing::error!(error = %e, "Audio output failed, retrying in 1s");
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
            Err(_) => {
                tracing::error!("Audio thread panicked, restarting in 1s");
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
        }
    }
}

fn run_cpal(
    stream: Arc<Mutex<Stream>>,
    time_provider: Arc<Mutex<TimeProvider>>,
    format: snapcast_proto::SampleFormat,
    volume: Arc<VolumeState>,
) -> anyhow::Result<()> {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or_else(|| anyhow::anyhow!("no output device"))?;

    tracing::info!(device = %device.name().unwrap_or_default(), "Using audio device");

    // Try to match stream format, fallback to default if unsupported
    let supported_formats = device.supported_output_configs()?;
    let mut target_config = None;
    for f in supported_formats {
        if f.channels() == format.channels() as u16
            && f.min_sample_rate().0 <= format.rate()
            && f.max_sample_rate().0 >= format.rate()
        {
            target_config = Some(f.with_sample_rate(cpal::SampleRate(format.rate())));
            break;
        }
    }

    let (config, use_resampler): (cpal::StreamConfig, bool) = if let Some(c) = target_config {
        (c.into(), false)
    } else {
        tracing::warn!("Stream format not supported by device, using default and resampling");
        let c = device.default_output_config()?;
        (c.into(), true)
    };

    let device_rate = config.sample_rate.0;
    let device_channels = config.channels as usize;

    #[cfg(feature = "resampler")]
    let mut resampler = if use_resampler {
        let device_format =
            snapcast_proto::SampleFormat::new(device_rate, 16, device_channels as u16);
        // We assume 20ms chunks for resampler init (typical for Snapcast)
        snapcast_client::resampler::Resampler::new_if_needed(
            format,
            device_format,
            (format.rate() / 50) as usize,
        )?
    } else {
        None
    };

    let stream_cb = Arc::clone(&stream);
    let tp_cb = Arc::clone(&time_provider);
    let cpal_stream = device.build_output_stream(
        &config,
        move |data: &mut [f32], info: &cpal::OutputCallbackInfo| {
            let num_frames = data.len() / device_channels;

            let buffer_dac_usec = info
                .timestamp()
                .playback
                .duration_since(&info.timestamp().callback)
                .map(|d| d.as_micros() as i64)
                .unwrap_or(0)
                + (num_frames as i64 * 1_000_000) / device_rate as i64;

            let server_now = {
                let tp = tp_cb.lock().unwrap_or_else(|e| e.into_inner());
                now_usec() + tp.diff_to_server_usec()
            };

            let mut s = stream_cb.lock().unwrap_or_else(|e| e.into_inner());
            let current_format = s.format();

            // Format change detection
            if current_format.rate() != format.rate()
                || current_format.channels() != format.channels()
            {
                // Return silence and hope the main loop picks up the change
                data.fill(0.0);
                return;
            }

            let frame_size = current_format.frame_size() as usize;
            let sample_size = current_format.sample_size() as usize;

            if frame_size == 0 {
                data.fill(0.0);
                return;
            }

            #[cfg(feature = "resampler")]
            if let Some(ref mut r) = resampler {
                // Resampling: we need to calculate how many input frames we need to get num_frames output
                // Rubato FftFixedIn is easier if we just process what we get.
                // For simplicity, we read a block from Stream, resample it, and buffer the rest.
                // But cpal callback must be fast.
                // A better approach is to have Stream return what it has and resample that.

                // For now, let's do a simple implementation that matches the requested output frames
                let input_frames =
                    (num_frames as f64 * format.rate() as f64 / device_rate as f64).ceil() as usize;
                let mut pcm_buf = vec![0u8; input_frames * frame_size];
                s.get_player_chunk_or_silence(
                    server_now,
                    buffer_dac_usec,
                    &mut pcm_buf,
                    input_frames as u32,
                );
                drop(s);

                if let Err(e) = r.process(&mut pcm_buf) {
                    tracing::error!(error = %e, "Resampling failed");
                    data.fill(0.0);
                    return;
                }

                // Convert resampled pcm_buf (which is in device_format i16) to f32 data
                // Note: Resampler currently outputs same sample size as input
                for (i, chunk) in pcm_buf.chunks_exact(2).enumerate() {
                    if i < data.len() {
                        data[i] = i16::from_le_bytes([chunk[0], chunk[1]]) as f32 / i16::MAX as f32;
                    }
                }
            } else {
                let mut pcm_buf = vec![0u8; num_frames * frame_size];
                s.get_player_chunk_or_silence(
                    server_now,
                    buffer_dac_usec,
                    &mut pcm_buf,
                    num_frames as u32,
                );
                drop(s);

                match sample_size {
                    2 => {
                        for (i, chunk) in pcm_buf.chunks_exact(2).enumerate() {
                            if i < data.len() {
                                data[i] = i16::from_le_bytes([chunk[0], chunk[1]]) as f32
                                    / i16::MAX as f32;
                            }
                        }
                    }
                    4 => {
                        for (i, chunk) in pcm_buf.chunks_exact(4).enumerate() {
                            if i < data.len() {
                                data[i] =
                                    f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                            }
                        }
                    }
                    _ => data.fill(0.0),
                }
            }
            #[cfg(not(feature = "resampler"))]
            {
                let mut pcm_buf = vec![0u8; num_frames * frame_size];
                s.get_player_chunk_or_silence(
                    server_now,
                    buffer_dac_usec,
                    &mut pcm_buf,
                    num_frames as u32,
                );
                drop(s);

                match sample_size {
                    2 => {
                        for (i, chunk) in pcm_buf.chunks_exact(2).enumerate() {
                            if i < data.len() {
                                data[i] = i16::from_le_bytes([chunk[0], chunk[1]]) as f32
                                    / i16::MAX as f32;
                            }
                        }
                    }
                    4 => {
                        for (i, chunk) in pcm_buf.chunks_exact(4).enumerate() {
                            if i < data.len() {
                                data[i] =
                                    f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                            }
                        }
                    }
                    _ => data.fill(0.0),
                }
            }

            // Apply software volume
            let gain = volume.gain();
            if gain < 1.0 {
                for sample in data.iter_mut() {
                    *sample *= gain;
                }
            }
        },
        |err| tracing::error!(error = %err, "Audio stream error"),
        None,
    )?;

    cpal_stream.play()?;
    tracing::info!("Audio playback started");

    loop {
        std::thread::sleep(std::time::Duration::from_millis(100));
        let s = stream.lock().unwrap_or_else(|e| e.into_inner());
        let current_format = s.format();
        if current_format.rate() != format.rate() || current_format.channels() != format.channels()
        {
            return Ok(());
        }
    }
}
