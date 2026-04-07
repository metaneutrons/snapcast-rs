//! ALSA player backend (Linux).
//!
//! Uses the alsa crate for low-level PCM output with precise buffer control.
//! Feature-gated behind the `alsa` feature.

use std::sync::{Arc, Mutex};
use std::thread;

use alsa::pcm::{Access, Format, HwParams, State};
use alsa::{Direction, PCM};
use anyhow::{Context, Result};
use snapcast_proto::SampleFormat;

use super::{Player, Volume, apply_volume};
use crate::config::PcmDevice;
use crate::stream::Stream;
use crate::time_provider::TimeProvider;

const BUFFER_TIME_MS: u64 = 80;
const FRAGMENTS: u32 = 4;

pub struct AlsaPlayer {
    stream: Arc<Mutex<Stream>>,
    time_provider: Arc<Mutex<TimeProvider>>,
    volume: Volume,
    sample_format: SampleFormat,
    device: String,
    active: Arc<std::sync::atomic::AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl AlsaPlayer {
    pub fn new(
        stream: Arc<Mutex<Stream>>,
        time_provider: Arc<Mutex<TimeProvider>>,
        sample_format: SampleFormat,
        device: &str,
    ) -> Self {
        Self {
            stream,
            time_provider,
            volume: Volume::default(),
            sample_format,
            device: device.to_string(),
            active: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            thread: None,
        }
    }

    /// List available ALSA PCM devices.
    pub fn pcm_list() -> Result<Vec<PcmDevice>> {
        let mut devices = Vec::new();
        let hints = alsa::device_name::HintIter::new(None, "pcm")?;
        for (idx, hint) in hints.enumerate() {
            let name = hint.name.unwrap_or_default();
            let desc = hint.desc.unwrap_or_default();
            if hint.direction != Some(alsa::Direction::Playback) && hint.direction.is_some() {
                continue;
            }
            devices.push(PcmDevice {
                idx: idx as i32,
                name,
                description: desc,
            });
        }
        Ok(devices)
    }
}

impl Player for AlsaPlayer {
    fn start(&mut self) -> Result<()> {
        self.active
            .store(true, std::sync::atomic::Ordering::Relaxed);

        let stream = Arc::clone(&self.stream);
        let time_provider = Arc::clone(&self.time_provider);
        let active = Arc::clone(&self.active);
        let format = self.sample_format;
        let volume = self.volume;
        let device = self.device.clone();

        let handle = thread::spawn(move || {
            if let Err(e) = alsa_worker(&device, format, stream, time_provider, active, volume) {
                tracing::error!("ALSA worker error: {e}");
            }
        });

        self.thread = Some(handle);
        tracing::info!(device = %self.device, "ALSA player started");
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        self.active
            .store(false, std::sync::atomic::Ordering::Relaxed);
        if let Some(handle) = self.thread.take() {
            handle.join().ok();
        }
        tracing::info!("ALSA player stopped");
        Ok(())
    }

    fn set_volume(&mut self, volume: Volume) {
        self.volume = volume;
    }

    fn volume(&self) -> Volume {
        self.volume
    }
}

impl Drop for AlsaPlayer {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

fn alsa_worker(
    device: &str,
    format: SampleFormat,
    stream: Arc<Mutex<Stream>>,
    time_provider: Arc<Mutex<TimeProvider>>,
    active: Arc<std::sync::atomic::AtomicBool>,
    volume: Volume,
) -> Result<()> {
    let pcm = PCM::new(device, Direction::Playback, false)
        .with_context(|| format!("opening ALSA device: {device}"))?;

    // Configure hardware parameters
    let hwp = HwParams::any(&pcm)?;
    hwp.set_access(Access::RWInterleaved)?;

    let alsa_format = match format.bits() {
        16 => Format::s16(),
        24 => Format::s24_3le(),
        32 => Format::s32(),
        _ => Format::s16(),
    };
    hwp.set_format(alsa_format)?;
    hwp.set_channels(format.channels() as u32)?;
    hwp.set_rate(format.rate(), alsa::ValueOr::Nearest)?;

    let buffer_time = BUFFER_TIME_MS * 1000; // microseconds
    let period_time = buffer_time / FRAGMENTS as u64;
    hwp.set_buffer_time_near(buffer_time as u32, alsa::ValueOr::Nearest)?;
    hwp.set_period_time_near(period_time as u32, alsa::ValueOr::Nearest)?;
    pcm.hw_params(&hwp)?;

    let period_size = pcm.hw_params_current()?.get_period_size()? as u32;
    let frame_size = format.frame_size() as usize;
    let buf_size = period_size as usize * frame_size;
    let mut buf = vec![0u8; buf_size];

    tracing::info!(
        period_frames = period_size,
        buffer_ms = BUFFER_TIME_MS,
        "ALSA configured"
    );

    pcm.start()?;

    while active.load(std::sync::atomic::Ordering::Relaxed) {
        // Estimate DAC delay from ALSA delay
        let delay_frames = pcm.delay().unwrap_or(0).max(0) as u64;
        let dac_time_usec = (delay_frames * 1_000_000) / format.rate() as u64;

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
            s.get_player_chunk_or_silence(server_now, dac_time_usec as i64, &mut buf, period_size);
        }

        apply_volume(&mut buf, format.sample_size(), &volume);

        let io = pcm.io_bytes();
        if let Err(e) = io.writei(&buf) {
            tracing::warn!("ALSA write error: {e}, recovering");
            pcm.recover(e.errno() as i32, true).ok();
        }
    }

    pcm.drain()?;
    Ok(())
}
