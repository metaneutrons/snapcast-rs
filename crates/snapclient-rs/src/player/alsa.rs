//! ALSA player backend (Linux).

use std::sync::{Arc, Mutex};
use std::thread;

use alsa::pcm::{Access, Format, HwParams};
use alsa::{Direction, PCM};
use anyhow::{Context, Result};
use snapcast_proto::SampleFormat;

use super::{Player, Volume, apply_volume};
use snapcast_client::config::PcmDevice;
use snapcast_client::stream::Stream;
use snapcast_client::time_provider::TimeProvider;

const BUFFER_TIME_US: u32 = 80_000;
const FRAGMENTS: u32 = 4;

pub struct AlsaPlayer {
    stream: Arc<Mutex<Stream>>,
    time_provider: Arc<Mutex<TimeProvider>>,
    volume: Arc<Mutex<Volume>>,
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
            volume: Arc::new(Mutex::new(Volume::default())),
            sample_format,
            device: device.to_string(),
            active: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            thread: None,
        }
    }

    pub fn pcm_list() -> Result<Vec<PcmDevice>> {
        let mut devices = Vec::new();
        let hints = alsa::device_name::HintIter::new(None, c"pcm")?;
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
        let volume = Arc::clone(&self.volume);
        let active = Arc::clone(&self.active);
        let format = self.sample_format;
        let device = self.device.clone();

        let handle = thread::spawn(move || {
            if let Err(e) = alsa_worker(&device, format, stream, time_provider, volume, active) {
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
        tracing::info!(device = %self.device, "ALSA player stopped");
        Ok(())
    }

    fn set_volume(&mut self, vol: Volume) {
        *self.volume.lock().unwrap() = vol;
    }

    fn volume(&self) -> Volume {
        *self.volume.lock().unwrap()
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
    volume: Arc<Mutex<Volume>>,
    active: Arc<std::sync::atomic::AtomicBool>,
) -> Result<()> {
    let pcm = PCM::new(device, Direction::Playback, false)
        .with_context(|| format!("opening ALSA device: {device}"))?;

    let hwp = HwParams::any(&pcm)?;
    hwp.set_access(Access::RWInterleaved)?;

    // 24-bit is padded to 4 bytes in our SampleFormat, so use S32 container
    let alsa_format = match format.bits() {
        16 => Format::s16(),
        24 => Format::s32(),
        32 => Format::s32(),
        _ => Format::s16(),
    };
    hwp.set_format(alsa_format)?;
    hwp.set_channels(format.channels() as u32)?;
    hwp.set_rate(format.rate(), alsa::ValueOr::Nearest)?;

    let period_time = BUFFER_TIME_US / FRAGMENTS;
    hwp.set_buffer_time_near(BUFFER_TIME_US, alsa::ValueOr::Nearest)?;
    hwp.set_period_time_near(period_time, alsa::ValueOr::Nearest)?;
    pcm.hw_params(&hwp)?;

    let period_size = pcm.hw_params_current()?.get_period_size()? as u32;
    let frame_size = format.frame_size() as usize;
    let buf_size = period_size as usize * frame_size;
    let mut buf = vec![0u8; buf_size];

    tracing::info!(
        period_frames = period_size,
        buffer_us = BUFFER_TIME_US,
        "ALSA configured"
    );

    while active.load(std::sync::atomic::Ordering::Relaxed) {
        let delay_frames = pcm.delay().unwrap_or(0).max(0) as u64;
        let dac_time_usec = (delay_frames * 1_000_000) / format.rate() as u64;

        let server_now = {
            let tp = time_provider.lock().unwrap();
            let now_usec = snapcast_client::connection::now_usec();
            now_usec + tp.diff_to_server_usec()
        };

        {
            let mut s = stream.lock().unwrap();
            s.get_player_chunk_or_silence(server_now, dac_time_usec as i64, &mut buf, period_size);
        }

        {
            let vol = volume.lock().unwrap();
            apply_volume(&mut buf, format.sample_size(), &vol);
        }

        let io = pcm.io_bytes();
        match io.writei(&buf) {
            Ok(_) => {}
            Err(e) => {
                tracing::warn!("ALSA write error: {e}, recovering");
                if pcm.recover(e.errno() as i32, true).is_err() {
                    tracing::error!("ALSA recovery failed, restarting");
                    break;
                }
            }
        }
    }

    pcm.drain().ok();
    Ok(())
}
