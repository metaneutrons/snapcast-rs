//! CoreAudio player backend (macOS).
//!
//! Uses the AudioUnit API to output PCM audio. The render callback pulls
//! time-synchronized chunks from the Stream and converts to f32.

use std::sync::{Arc, Mutex};

use anyhow::Result;
use coreaudio::audio_unit::render_callback::{self, data};
use coreaudio::audio_unit::{AudioUnit, IOType};
use snapcast_proto::SampleFormat;

use super::{Player, Volume, apply_volume};
use crate::stream::Stream;
use crate::time_provider::TimeProvider;

/// CoreAudio player using AudioUnit output.
pub struct CoreAudioPlayer {
    audio_unit: Option<AudioUnit>,
    stream: Arc<Mutex<Stream>>,
    time_provider: Arc<Mutex<TimeProvider>>,
    volume: Volume,
    sample_format: SampleFormat,
}

impl CoreAudioPlayer {
    pub fn new(
        stream: Arc<Mutex<Stream>>,
        time_provider: Arc<Mutex<TimeProvider>>,
        sample_format: SampleFormat,
    ) -> Self {
        Self {
            audio_unit: None,
            stream,
            time_provider,
            volume: Volume::default(),
            sample_format,
        }
    }
}

impl Player for CoreAudioPlayer {
    fn start(&mut self) -> Result<()> {
        let mut audio_unit = AudioUnit::new(IOType::DefaultOutput)?;

        let format = self.sample_format;
        let stream = Arc::clone(&self.stream);
        let time_provider = Arc::clone(&self.time_provider);
        let sample_size = format.sample_size();
        let volume = self.volume;
        let dac_delay_usec: i64 = 15_000;

        // CoreAudio default output uses f32 non-interleaved
        type Args = render_callback::Args<data::NonInterleaved<f32>>;

        audio_unit.set_render_callback(move |args: Args| {
            let Args {
                num_frames,
                mut data,
                ..
            } = args;

            let frame_size = format.frame_size() as usize;
            let buf_size = num_frames * frame_size;
            let mut pcm_buf = vec![0u8; buf_size];

            let buffer_dac_usec =
                (num_frames as i64 * 1_000_000) / format.rate() as i64 + dac_delay_usec;

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
                s.get_player_chunk_or_silence(
                    server_now,
                    buffer_dac_usec,
                    &mut pcm_buf,
                    num_frames as u32,
                );
            }

            apply_volume(&mut pcm_buf, sample_size, &volume);

            // Convert interleaved i16 LE PCM to f32 non-interleaved for CoreAudio
            let channels = format.channels() as usize;
            for i in 0..num_frames {
                for (ch, channel) in data.channels_mut().enumerate() {
                    if ch >= channels {
                        break;
                    }
                    let byte_offset = (i * channels + ch) * 2;
                    let sample =
                        i16::from_le_bytes([pcm_buf[byte_offset], pcm_buf[byte_offset + 1]]);
                    channel[i] = sample as f32 / i16::MAX as f32;
                }
            }

            Ok(())
        })?;

        audio_unit.start()?;
        tracing::info!("CoreAudio player started");
        self.audio_unit = Some(audio_unit);
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        if let Some(ref mut au) = self.audio_unit {
            au.stop()?;
        }
        self.audio_unit = None;
        tracing::info!("CoreAudio player stopped");
        Ok(())
    }

    fn set_volume(&mut self, volume: Volume) {
        self.volume = volume;
    }

    fn volume(&self) -> Volume {
        self.volume
    }
}

impl Drop for CoreAudioPlayer {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}
