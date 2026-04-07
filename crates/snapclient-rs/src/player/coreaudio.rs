//! CoreAudio player backend (macOS).

use std::sync::{Arc, Mutex};

use anyhow::Result;
use coreaudio::audio_unit::render_callback::{self, data};
use coreaudio::audio_unit::{AudioUnit, IOType};
use snapcast_proto::SampleFormat;

use super::{Player, Volume, apply_volume};
use crate::stream::Stream;
use crate::time_provider::TimeProvider;

pub struct CoreAudioPlayer {
    audio_unit: Option<AudioUnit>,
    stream: Arc<Mutex<Stream>>,
    time_provider: Arc<Mutex<TimeProvider>>,
    volume: Arc<Mutex<Volume>>,
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
            volume: Arc::new(Mutex::new(Volume::default())),
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
        let volume = Arc::clone(&self.volume);
        let sample_size = format.sample_size() as usize;
        let channels = format.channels() as usize;
        let frame_size = format.frame_size() as usize;
        let dac_delay_usec: i64 = 15_000;

        // Pre-allocate buffer outside callback
        let pcm_buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        let pcm_buf_clone = Arc::clone(&pcm_buf);

        type Args = render_callback::Args<data::NonInterleaved<f32>>;

        audio_unit.set_render_callback(move |args: Args| {
            let Args {
                num_frames,
                mut data,
                ..
            } = args;

            let buf_size = num_frames * frame_size;
            let mut buf = pcm_buf_clone.lock().unwrap();
            buf.resize(buf_size, 0);

            let buffer_dac_usec =
                (num_frames as i64 * 1_000_000) / format.rate() as i64 + dac_delay_usec;

            let server_now = {
                let tp = time_provider.lock().unwrap();
                let now_usec = crate::connection::now_usec();
                now_usec + tp.diff_to_server_usec()
            };

            {
                let mut s = stream.lock().unwrap();
                s.get_player_chunk_or_silence(
                    server_now,
                    buffer_dac_usec,
                    &mut buf,
                    num_frames as u32,
                );
            }

            {
                let vol = volume.lock().unwrap();
                apply_volume(&mut buf, format.sample_size(), &vol);
            }

            // Convert interleaved PCM to f32 non-interleaved for CoreAudio
            for i in 0..num_frames {
                for (ch, channel) in data.channels_mut().enumerate() {
                    if ch >= channels {
                        break;
                    }
                    let byte_offset = (i * channels + ch) * sample_size;
                    let sample_f32 = match sample_size {
                        2 => {
                            let s = i16::from_le_bytes([buf[byte_offset], buf[byte_offset + 1]]);
                            s as f32 / i16::MAX as f32
                        }
                        4 => {
                            let s = i32::from_le_bytes([
                                buf[byte_offset],
                                buf[byte_offset + 1],
                                buf[byte_offset + 2],
                                buf[byte_offset + 3],
                            ]);
                            s as f32 / i32::MAX as f32
                        }
                        _ => 0.0,
                    };
                    channel[i] = sample_f32;
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

    fn set_volume(&mut self, vol: Volume) {
        *self.volume.lock().unwrap() = vol;
    }

    fn volume(&self) -> Volume {
        *self.volume.lock().unwrap()
    }
}

impl Drop for CoreAudioPlayer {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}
