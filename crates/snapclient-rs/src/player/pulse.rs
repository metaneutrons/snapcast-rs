//! PulseAudio player backend (Linux).
//!
//! Uses libpulse-simple-binding for straightforward blocking PCM output.
//! Feature-gated behind the `pulse` feature.

use std::sync::{Arc, Mutex};
use std::thread;

use anyhow::{Context, Result};
use libpulse_binding::sample::{Format, Spec};
use libpulse_binding::stream::Direction;
use libpulse_simple_binding::Simple;
use snapcast_proto::SampleFormat;

use super::{Player, Volume, apply_volume};
use crate::stream::Stream;
use crate::time_provider::TimeProvider;

const BUFFER_TIME_MS: u64 = 100;

pub struct PulsePlayer {
    stream: Arc<Mutex<Stream>>,
    time_provider: Arc<Mutex<TimeProvider>>,
    volume: Volume,
    sample_format: SampleFormat,
    server: Option<String>,
    active: Arc<std::sync::atomic::AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl PulsePlayer {
    pub fn new(
        stream: Arc<Mutex<Stream>>,
        time_provider: Arc<Mutex<TimeProvider>>,
        sample_format: SampleFormat,
        server: Option<String>,
    ) -> Self {
        Self {
            stream,
            time_provider,
            volume: Volume::default(),
            sample_format,
            server,
            active: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            thread: None,
        }
    }
}

impl Player for PulsePlayer {
    fn start(&mut self) -> Result<()> {
        self.active
            .store(true, std::sync::atomic::Ordering::Relaxed);

        let stream = Arc::clone(&self.stream);
        let time_provider = Arc::clone(&self.time_provider);
        let active = Arc::clone(&self.active);
        let format = self.sample_format;
        let volume = self.volume;
        let server = self.server.clone();

        let handle = thread::spawn(move || {
            if let Err(e) = pulse_worker(format, stream, time_provider, active, volume, server) {
                tracing::error!("PulseAudio worker error: {e}");
            }
        });

        self.thread = Some(handle);
        tracing::info!("PulseAudio player started");
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        self.active
            .store(false, std::sync::atomic::Ordering::Relaxed);
        if let Some(handle) = self.thread.take() {
            handle.join().ok();
        }
        tracing::info!("PulseAudio player stopped");
        Ok(())
    }

    fn set_volume(&mut self, volume: Volume) {
        self.volume = volume;
    }

    fn volume(&self) -> Volume {
        self.volume
    }
}

impl Drop for PulsePlayer {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

fn pulse_worker(
    format: SampleFormat,
    stream: Arc<Mutex<Stream>>,
    time_provider: Arc<Mutex<TimeProvider>>,
    active: Arc<std::sync::atomic::AtomicBool>,
    volume: Volume,
    server: Option<String>,
) -> Result<()> {
    let pulse_format = match format.bits() {
        16 => Format::S16le,
        24 => Format::S24_32le,
        32 => Format::S32le,
        _ => Format::S16le,
    };

    let spec = Spec {
        format: pulse_format,
        channels: format.channels() as u8,
        rate: format.rate(),
    };

    if !spec.is_valid() {
        anyhow::bail!("invalid PulseAudio sample spec: {:?}", spec);
    }

    let server_str = server.as_deref();
    let simple = Simple::new(
        server_str,
        "snapclient-rs",
        Direction::Playback,
        None,
        "playback",
        &spec,
        None,
        None,
    )
    .map_err(|e| anyhow::anyhow!("PulseAudio connect: {e}"))?;

    let frames_per_chunk = (format.rate() as u64 * BUFFER_TIME_MS / 1000) as u32;
    let frame_size = format.frame_size() as usize;
    let buf_size = frames_per_chunk as usize * frame_size;
    let mut buf = vec![0u8; buf_size];

    // Estimate buffer latency
    let buffer_usec = BUFFER_TIME_MS * 1000;

    tracing::info!(
        frames = frames_per_chunk,
        buffer_ms = BUFFER_TIME_MS,
        "PulseAudio configured"
    );

    while active.load(std::sync::atomic::Ordering::Relaxed) {
        let latency_usec = simple
            .get_latency()
            .map(|l| l.as_micros() as i64)
            .unwrap_or(buffer_usec as i64);

        let server_now = {
            let tp = time_provider.lock().unwrap();
            let now_usec = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_micros() as i64;
            now_usec + tp.diff_to_server_usec()
        };

        {
            let mut s = stream.lock().unwrap();
            s.get_player_chunk_or_silence(server_now, latency_usec, &mut buf, frames_per_chunk);
        }

        apply_volume(&mut buf, format.sample_size(), &volume);

        if let Err(e) = simple.write(&buf) {
            tracing::warn!("PulseAudio write error: {e}");
            break;
        }
    }

    simple.drain().ok();
    Ok(())
}
