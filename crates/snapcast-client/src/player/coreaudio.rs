//! CoreAudio player backend (macOS).

use std::sync::{Arc, Mutex};

use anyhow::Result;
use coreaudio::audio_unit::render_callback::{self, data};
use coreaudio::audio_unit::{AudioUnit, Element, IOType, Scope};
use snapcast_proto::SampleFormat;

use super::{Player, Volume, apply_volume};
use crate::stream::Stream;
use crate::time_provider::TimeProvider;

/// Query the total output latency of the AudioUnit in microseconds.
/// Includes: AudioUnit processing latency + one callback buffer period (double-buffering).
/// This matches the C++ approach: buffered frames + DAC delay.
fn query_output_latency(audio_unit: &AudioUnit, sample_rate: u32) -> i64 {
    // kAudioUnitProperty_Latency = 12 (returns seconds as f64)
    let au_latency_usec = audio_unit
        .get_property::<f64>(12, Scope::Global, Element::Output)
        .map(|secs| (secs * 1_000_000.0) as i64)
        .unwrap_or(0);

    // kAudioDevicePropertyLatency = 0x6C746E63 ('ltnc') on the output device
    // kAudioDevicePropertySafetyOffset = 0x73616674 ('saft')
    // kAudioDevicePropertyBufferFrameSize = 0x6673697A ('fsiz')
    // These require the device ID which we get from the AudioUnit
    let device_latency_frames: i64 = audio_unit
        .get_property::<u32>(0x6C746E63, Scope::Output, Element::Output)
        .map(|f| f as i64)
        .unwrap_or(0);

    let safety_offset_frames: i64 = audio_unit
        .get_property::<u32>(0x73616674, Scope::Output, Element::Output)
        .map(|f| f as i64)
        .unwrap_or(0);

    let buffer_frames: i64 = audio_unit
        .get_property::<u32>(0x6673697A, Scope::Global, Element::Output)
        .map(|f| f as i64)
        .unwrap_or(512);

    let frames_to_usec = |frames: i64| frames * 1_000_000 / sample_rate as i64;

    // Total latency: AU processing + device hardware + safety offset + one buffer period
    // The one buffer period accounts for AudioUnit's internal double-buffering
    let total = au_latency_usec
        + frames_to_usec(device_latency_frames)
        + frames_to_usec(safety_offset_frames)
        + frames_to_usec(buffer_frames);

    tracing::info!(
        au_latency_us = au_latency_usec,
        device_latency_frames,
        safety_offset_frames,
        buffer_frames,
        total_us = total,
        "CoreAudio output latency"
    );

    total
}

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

        // Set sample rate to match our stream
        let mut stream_format = audio_unit.output_stream_format()?;
        stream_format.sample_rate = format.rate() as f64;
        audio_unit.set_property(
            8, // kAudioUnitProperty_StreamFormat
            Scope::Input,
            Element::Output,
            Some(&stream_format.to_asbd()),
        )?;

        // Query actual output latency for precise DAC delay estimation
        let dac_delay_usec = query_output_latency(&audio_unit, format.rate());

        let stream = Arc::clone(&self.stream);
        let time_provider = Arc::clone(&self.time_provider);
        let volume = Arc::clone(&self.volume);
        let sample_size = format.sample_size() as usize;
        let channels = format.channels() as usize;
        let frame_size = format.frame_size() as usize;

        let mut pcm_buf: Vec<u8> = Vec::new();

        type Args = render_callback::Args<data::NonInterleaved<f32>>;

        audio_unit.set_render_callback(move |args: Args| {
            let Args {
                num_frames,
                mut data,
                ..
            } = args;

            pcm_buf.resize(num_frames * frame_size, 0);

            // Total time until this buffer reaches the speakers:
            // current buffer duration + pipeline latency
            let buffer_dac_usec =
                (num_frames as i64 * 1_000_000) / format.rate() as i64 + dac_delay_usec;

            let server_now = {
                let tp = time_provider.lock().unwrap();
                crate::connection::now_usec() + tp.diff_to_server_usec()
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

            {
                let vol = volume.lock().unwrap();
                apply_volume(&mut pcm_buf, format.sample_size(), &vol);
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
                            let s = i16::from_le_bytes([
                                pcm_buf[byte_offset],
                                pcm_buf[byte_offset + 1],
                            ]);
                            s as f32 / i16::MAX as f32
                        }
                        4 => {
                            let s = i32::from_le_bytes([
                                pcm_buf[byte_offset],
                                pcm_buf[byte_offset + 1],
                                pcm_buf[byte_offset + 2],
                                pcm_buf[byte_offset + 3],
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
