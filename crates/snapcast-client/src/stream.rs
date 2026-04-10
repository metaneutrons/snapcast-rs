//! Time-synchronized PCM audio stream buffer.

use std::collections::VecDeque;

use snapcast_proto::SampleFormat;
use snapcast_proto::types::Timeval;

use crate::double_buffer::DoubleBuffer;

/// A decoded PCM chunk with a server-time timestamp and a read cursor.
#[derive(Debug, Clone)]
pub struct PcmChunk {
    /// Server-time timestamp of this chunk.
    pub timestamp: Timeval,
    /// Raw PCM sample data.
    pub data: Vec<u8>,
    /// Sample format (rate, bits, channels).
    pub format: SampleFormat,
    read_pos: usize,
}

impl PcmChunk {
    /// Create a new PCM chunk.
    pub fn new(timestamp: Timeval, data: Vec<u8>, format: SampleFormat) -> Self {
        Self {
            timestamp,
            data,
            format,
            read_pos: 0,
        }
    }

    /// Start time of this chunk in microseconds.
    pub fn start_usec(&self) -> i64 {
        self.timestamp.to_usec()
    }

    /// Duration of this chunk in microseconds.
    pub fn duration_usec(&self) -> i64 {
        if self.format.frame_size() == 0 || self.format.rate() == 0 {
            return 0;
        }
        let frames = self.data.len() as i64 / self.format.frame_size() as i64;
        frames * 1_000_000 / self.format.rate() as i64
    }

    /// Read up to `frames` frames into `output`, returning the number read.
    pub fn read_frames(&mut self, output: &mut [u8], frames: u32) -> u32 {
        let frame_size = self.format.frame_size() as usize;
        let available_bytes = self.data.len() - self.read_pos;
        let available_frames = available_bytes / frame_size;
        let to_read = (frames as usize).min(available_frames);
        let bytes = to_read * frame_size;
        output[..bytes].copy_from_slice(&self.data[self.read_pos..self.read_pos + bytes]);
        self.read_pos += bytes;
        to_read as u32
    }

    /// Returns true if all data has been read.
    pub fn is_end(&self) -> bool {
        self.read_pos >= self.data.len()
    }

    /// Skip forward by `frames` frames.
    pub fn seek(&mut self, frames: u32) {
        let bytes = frames as usize * self.format.frame_size() as usize;
        self.read_pos = (self.read_pos + bytes).min(self.data.len());
    }
}

/// Correction threshold — soft sync starts when |short_median| > 100µs
const CORRECTION_BEGIN_USEC: i64 = 100;
/// Hard sync: |median| exceeds this (µs).
const HARD_SYNC_MEDIAN_USEC: i64 = 2000;
/// Hard sync: |short_median| exceeds this (µs).
const HARD_SYNC_SHORT_MEDIAN_USEC: i64 = 5000;
/// Hard sync: |mini_median| exceeds this (µs).
const HARD_SYNC_MINI_MEDIAN_USEC: i64 = 50000;
/// Hard sync: |age| exceeds this (µs).
const HARD_SYNC_AGE_USEC: i64 = 500_000;
/// Minimum |age| for hard sync re-trigger (µs).
const HARD_SYNC_MIN_AGE_USEC: i64 = 500;
/// Minimum |mini_median| for soft sync (µs).
const SOFT_SYNC_MIN_USEC: i64 = 50;
/// Maximum playback rate correction factor.
const MAX_RATE_CORRECTION: f64 = 0.0005;
/// Rate correction scaling factor.
const RATE_CORRECTION_SCALE: f64 = 0.00005;

/// Time-synchronized PCM stream buffer.
pub struct Stream {
    format: SampleFormat,
    chunks: VecDeque<PcmChunk>,
    current: Option<PcmChunk>,
    buffer_ms: i64,
    hard_sync: bool,

    // Drift detection
    mini_buffer: DoubleBuffer,
    short_buffer: DoubleBuffer,
    buffer: DoubleBuffer,
    median: i64,
    short_median: i64,

    // Soft sync
    played_frames: u32,
    correct_after_x_frames: i32,
    frame_delta: i32,
    read_buf: Vec<u8>,

    // Stats
    last_log_sec: i64,
}

impl Stream {
    /// Create a new stream for the given sample format.
    pub fn new(format: SampleFormat) -> Self {
        Self {
            format,
            chunks: VecDeque::new(),
            current: None,
            buffer_ms: 1000,
            hard_sync: true,
            mini_buffer: DoubleBuffer::new(20),
            short_buffer: DoubleBuffer::new(100),
            buffer: DoubleBuffer::new(500),
            median: 0,
            short_median: 0,
            played_frames: 0,
            correct_after_x_frames: 0,
            frame_delta: 0,
            read_buf: Vec::new(),
            last_log_sec: 0,
        }
    }

    /// Returns the sample format.
    pub fn format(&self) -> SampleFormat {
        self.format
    }

    /// Set the target buffer size in milliseconds.
    pub fn set_buffer_ms(&mut self, ms: i64) {
        self.buffer_ms = ms;
    }

    /// Enqueue a decoded PCM chunk.
    pub fn add_chunk(&mut self, chunk: PcmChunk) {
        self.chunks.push_back(chunk);
    }

    /// Number of queued chunks.
    pub fn chunk_count(&self) -> usize {
        self.chunks.len()
    }

    /// Clear all queued chunks and reset sync state.
    pub fn clear(&mut self) {
        self.chunks.clear();
        self.current = None;
        self.hard_sync = true;
    }

    fn reset_buffers(&mut self) {
        self.buffer.clear();
        self.mini_buffer.clear();
        self.short_buffer.clear();
    }

    fn update_buffers(&mut self, age: i64) {
        self.buffer.add(age);
        self.mini_buffer.add(age);
        self.short_buffer.add(age);
    }

    fn set_real_sample_rate(&mut self, sample_rate: f64) {
        let nominal = self.format.rate() as f64;
        if (sample_rate - nominal).abs() < f64::EPSILON {
            self.correct_after_x_frames = 0;
        } else {
            let ratio = nominal / sample_rate;
            self.correct_after_x_frames = (ratio / (ratio - 1.0)).round() as i32;
        }
    }

    /// Fill `output` with time-synchronized PCM data. Returns false if no data available.
    pub fn get_player_chunk(
        &mut self,
        server_now_usec: i64,
        output_buffer_dac_time_usec: i64,
        output: &mut [u8],
        frames: u32,
    ) -> bool {
        let needs_new = self.current.as_ref().is_none_or(|c| c.is_end());
        if needs_new {
            self.current = self.chunks.pop_front();
        }
        if self.current.is_none() {
            return false;
        }

        // --- Hard sync: initial alignment ---
        if self.hard_sync {
            let chunk = self.current.as_ref().unwrap();
            let req_duration_usec = (frames as i64 * 1_000_000) / self.format.rate() as i64;
            let age_usec = server_now_usec - chunk.start_usec() - self.buffer_ms * 1000
                + output_buffer_dac_time_usec;

            if age_usec < -req_duration_usec {
                self.get_silence(output, frames);
                return true;
            }

            if age_usec > 0 {
                self.current = None;
                while let Some(mut c) = self.chunks.pop_front() {
                    let a = server_now_usec - c.start_usec() - self.buffer_ms * 1000
                        + output_buffer_dac_time_usec;
                    if a > 0 && a < c.duration_usec() {
                        let skip = (self.format.rate() as f64 * a as f64 / 1_000_000.0) as u32;
                        c.seek(skip);
                        self.current = Some(c);
                        break;
                    } else if a <= 0 {
                        self.current = Some(c);
                        break;
                    }
                }
                if self.current.is_none() {
                    return false;
                }
            }

            let chunk = self.current.as_ref().unwrap();
            let age_usec = server_now_usec - chunk.start_usec() - self.buffer_ms * 1000
                + output_buffer_dac_time_usec;

            if age_usec <= 0 {
                let silent_frames =
                    (self.format.rate() as f64 * (-age_usec) as f64 / 1_000_000.0) as u32;
                let silent_frames = silent_frames.min(frames);
                let frame_size = self.format.frame_size() as usize;

                if silent_frames > 0 {
                    output[..silent_frames as usize * frame_size].fill(0);
                }
                let remaining = frames - silent_frames;
                if remaining > 0 {
                    let offset = silent_frames as usize * frame_size;
                    self.read_next(&mut output[offset..], remaining);
                }
                if silent_frames < frames {
                    self.hard_sync = false;
                    self.reset_buffers();
                }
                return true;
            }
            return false;
        }

        // --- Normal playback with drift correction ---

        // Compute frames correction from current rate adjustment
        let mut frames_correction: i32 = 0;
        if self.correct_after_x_frames != 0 {
            self.played_frames += frames;
            if self.played_frames >= self.correct_after_x_frames.unsigned_abs() {
                frames_correction = self.played_frames as i32 / self.correct_after_x_frames;
                self.played_frames %= self.correct_after_x_frames.unsigned_abs();
            }
        }

        // Read with correction (or plain read if correction == 0)
        let chunk_start = match self.read_with_correction(output, frames, frames_correction) {
            Some(ts) => ts,
            None => return false,
        };

        let age_usec =
            server_now_usec - chunk_start - self.buffer_ms * 1000 + output_buffer_dac_time_usec;

        // Reset sample rate to nominal, soft sync may override below
        self.set_real_sample_rate(self.format.rate() as f64);

        // Hard sync re-trigger thresholds (matching C++)
        if self.buffer.full()
            && self.median.abs() > HARD_SYNC_MEDIAN_USEC
            && age_usec.abs() > HARD_SYNC_MIN_AGE_USEC
        {
            tracing::info!(
                median = self.median,
                "Hard sync: buffer full, |median| > 2ms"
            );
            self.hard_sync = true;
        } else if self.short_buffer.full()
            && self.short_median.abs() > HARD_SYNC_SHORT_MEDIAN_USEC
            && age_usec.abs() > HARD_SYNC_MIN_AGE_USEC
        {
            tracing::info!(
                short_median = self.short_median,
                "Hard sync: short buffer full, |short_median| > 5ms"
            );
            self.hard_sync = true;
        } else if self.mini_buffer.full()
            && self.mini_buffer.median_simple().abs() > HARD_SYNC_MINI_MEDIAN_USEC
            && age_usec.abs() > HARD_SYNC_MIN_AGE_USEC
        {
            tracing::info!("Hard sync: mini buffer full, |mini_median| > 50ms");
            self.hard_sync = true;
        } else if age_usec.abs() > HARD_SYNC_AGE_USEC {
            tracing::info!(age_usec, "Hard sync: |age| > 500ms");
            self.hard_sync = true;
        } else if self.short_buffer.full() {
            // Soft sync: adjust playback speed based on drift
            let mini_median = self.mini_buffer.median_simple();
            if self.short_median > CORRECTION_BEGIN_USEC
                && mini_median > SOFT_SYNC_MIN_USEC
                && age_usec > SOFT_SYNC_MIN_USEC
            {
                let rate = (self.short_median as f64 / 100.0) * RATE_CORRECTION_SCALE;
                let rate = 1.0 - rate.min(MAX_RATE_CORRECTION);
                self.set_real_sample_rate(self.format.rate() as f64 * rate);
            } else if self.short_median < -CORRECTION_BEGIN_USEC
                && mini_median < -SOFT_SYNC_MIN_USEC
                && age_usec < -SOFT_SYNC_MIN_USEC
            {
                let rate = (-self.short_median as f64 / 100.0) * RATE_CORRECTION_SCALE;
                let rate = 1.0 + rate.min(MAX_RATE_CORRECTION);
                self.set_real_sample_rate(self.format.rate() as f64 * rate);
            }
        }

        self.update_buffers(age_usec);

        // Stats logging (once per second)
        let now_sec = server_now_usec / 1_000_000;
        if now_sec != self.last_log_sec {
            self.last_log_sec = now_sec;
            self.median = self.buffer.median_simple();
            self.short_median = self.short_buffer.median_simple();
            tracing::debug!(
                target: "Stats",
                "Chunk: {}\t{}\t{}\t{}\t{}\t{}\t{}",
                age_usec,
                self.mini_buffer.median_simple(),
                self.short_median,
                self.median,
                self.buffer.len(),
                output_buffer_dac_time_usec / 1000,
                self.frame_delta,
            );
            self.frame_delta = 0;
        }

        age_usec.abs() < 500_000
    }

    /// Fill `output` with silence.
    pub fn get_silence(&self, output: &mut [u8], frames: u32) {
        let bytes = frames as usize * self.format.frame_size() as usize;
        output[..bytes].fill(0);
    }

    /// Like [`get_player_chunk`](Self::get_player_chunk), but fills silence on failure.
    pub fn get_player_chunk_or_silence(
        &mut self,
        server_now_usec: i64,
        output_buffer_dac_time_usec: i64,
        output: &mut [u8],
        frames: u32,
    ) -> bool {
        let result =
            self.get_player_chunk(server_now_usec, output_buffer_dac_time_usec, output, frames);
        if !result {
            self.get_silence(output, frames);
        }
        result
    }

    fn read_next(&mut self, output: &mut [u8], frames: u32) -> Option<i64> {
        let chunk = self.current.as_mut()?;
        // Adjusted timestamp: chunk start + already-consumed frames
        let frame_size = self.format.frame_size() as usize;
        let consumed_frames = chunk.read_pos / frame_size;
        let ts =
            chunk.start_usec() + consumed_frames as i64 * 1_000_000 / self.format.rate() as i64;
        let mut read = 0u32;
        while read < frames {
            let offset = read as usize * frame_size;
            let n = chunk.read_frames(&mut output[offset..], frames - read);
            read += n;
            if read < frames && chunk.is_end() {
                match self.chunks.pop_front() {
                    Some(next) => *chunk = next,
                    None => break,
                }
            }
        }
        Some(ts)
    }

    fn read_with_correction(
        &mut self,
        output: &mut [u8],
        frames: u32,
        correction: i32,
    ) -> Option<i64> {
        if correction == 0 {
            return self.read_next(output, frames);
        }

        // Clamp correction to avoid underflow
        let correction = correction.max(-(frames as i32) + 1);

        self.frame_delta -= correction;
        let to_read = (frames as i32 + correction) as u32;
        let frame_size = self.format.frame_size() as usize;

        self.read_buf.resize(to_read as usize * frame_size, 0);
        let mut read_buf = std::mem::take(&mut self.read_buf);
        let ts = self.read_next(&mut read_buf, to_read);

        let max = if correction < 0 {
            frames as usize
        } else {
            to_read as usize
        };
        let slices = (correction.unsigned_abs() as usize + 1).min(max);
        let slice_size = max / slices;

        let mut pos = 0usize;
        for n in 0..slices {
            let size = if n + 1 == slices {
                max - pos
            } else {
                slice_size
            };

            if correction < 0 {
                let src_start = (pos - n) * frame_size;
                let dst_start = pos * frame_size;
                let len = size * frame_size;
                output[dst_start..dst_start + len]
                    .copy_from_slice(&read_buf[src_start..src_start + len]);
            } else {
                let src_start = pos * frame_size;
                let dst_start = (pos - n) * frame_size;
                let len = size * frame_size;
                output[dst_start..dst_start + len]
                    .copy_from_slice(&read_buf[src_start..src_start + len]);
            }
            pos += size;
        }

        self.read_buf = read_buf;
        ts
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fmt() -> SampleFormat {
        SampleFormat::new(48000, 16, 2)
    }

    fn make_chunk(sec: i32, usec: i32, frames: u32, format: SampleFormat) -> PcmChunk {
        let bytes = frames as usize * format.frame_size() as usize;
        let data: Vec<u8> = (0..bytes).map(|i| (i % 256) as u8).collect();
        PcmChunk::new(Timeval { sec, usec }, data, format)
    }

    #[test]
    fn pcm_chunk_duration() {
        let f = fmt();
        let chunk = make_chunk(0, 0, 480, f);
        assert_eq!(chunk.duration_usec(), 10_000);
    }

    #[test]
    fn pcm_chunk_read_frames() {
        let f = fmt();
        let mut chunk = make_chunk(0, 0, 100, f);
        let mut buf = vec![0u8; 50 * f.frame_size() as usize];
        let read = chunk.read_frames(&mut buf, 50);
        assert_eq!(read, 50);
        assert!(!chunk.is_end());
        let read = chunk.read_frames(&mut buf, 50);
        assert_eq!(read, 50);
        assert!(chunk.is_end());
    }

    #[test]
    fn pcm_chunk_seek() {
        let f = fmt();
        let mut chunk = make_chunk(0, 0, 100, f);
        chunk.seek(90);
        let mut buf = vec![0u8; 100 * f.frame_size() as usize];
        let read = chunk.read_frames(&mut buf, 100);
        assert_eq!(read, 10);
    }

    #[test]
    fn stream_add_and_count() {
        let f = fmt();
        let mut stream = Stream::new(f);
        assert_eq!(stream.chunk_count(), 0);
        stream.add_chunk(make_chunk(100, 0, 480, f));
        stream.add_chunk(make_chunk(100, 10_000, 480, f));
        assert_eq!(stream.chunk_count(), 2);
    }

    #[test]
    fn stream_clear() {
        let f = fmt();
        let mut stream = Stream::new(f);
        stream.add_chunk(make_chunk(100, 0, 480, f));
        stream.clear();
        assert_eq!(stream.chunk_count(), 0);
    }

    #[test]
    fn stream_silence_when_empty() {
        let f = fmt();
        let mut stream = Stream::new(f);
        let mut buf = vec![0xFFu8; 480 * f.frame_size() as usize];
        let result = stream.get_player_chunk(100_000_000, 0, &mut buf, 480);
        assert!(!result);
    }

    #[test]
    fn stream_hard_sync_plays_silence_when_too_early() {
        let f = fmt();
        let mut stream = Stream::new(f);
        stream.set_buffer_ms(1000);
        stream.add_chunk(make_chunk(100, 0, 4800, f));
        let server_now = 100_000_000i64;
        let mut buf = vec![0xFFu8; 480 * f.frame_size() as usize];
        let result = stream.get_player_chunk(server_now, 0, &mut buf, 480);
        assert!(result);
        assert!(buf.iter().all(|&b| b == 0));
    }

    #[test]
    fn stream_hard_sync_plays_data_when_aligned() {
        let f = fmt();
        let mut stream = Stream::new(f);
        stream.set_buffer_ms(1000);
        stream.add_chunk(make_chunk(99, 0, 4800, f));
        let server_now = 100_000_000i64;
        let mut buf = vec![0u8; 480 * f.frame_size() as usize];
        let result = stream.get_player_chunk(server_now, 0, &mut buf, 480);
        assert!(result);
        assert!(buf.iter().any(|&b| b != 0));
    }

    #[test]
    fn set_real_sample_rate_correction() {
        let f = fmt();
        let mut stream = Stream::new(f);
        stream.set_real_sample_rate(48000.0);
        assert_eq!(stream.correct_after_x_frames, 0);

        stream.set_real_sample_rate(47999.0);
        assert_ne!(stream.correct_after_x_frames, 0);
    }

    #[test]
    fn read_with_correction_remove_one_frame() {
        let f = fmt(); // 48000:16:2, frame_size=4
        let mut stream = Stream::new(f);

        let mut data = Vec::new();
        for i in 0..10u16 {
            data.extend_from_slice(&i.to_le_bytes());
            data.extend_from_slice(&(i + 100).to_le_bytes());
        }
        stream.add_chunk(make_chunk(100, 0, 10, f));
        stream.chunks.back_mut().unwrap().data = data;
        stream.current = stream.chunks.pop_front();

        let mut output = vec![0u8; 9 * f.frame_size() as usize];
        let ts = stream.read_with_correction(&mut output, 9, 1);
        assert!(ts.is_some());
        assert_eq!(output.len(), 36);
        for (i, chunk) in output.chunks(4).enumerate() {
            let left = u16::from_le_bytes([chunk[0], chunk[1]]);
            assert!(left <= 10, "frame {i}: left={left}");
        }
    }

    #[test]
    fn read_with_correction_zero_is_passthrough() {
        let f = fmt();
        let mut stream = Stream::new(f);
        stream.add_chunk(make_chunk(100, 0, 100, f));
        stream.current = stream.chunks.pop_front();

        let mut out1 = vec![0u8; 50 * f.frame_size() as usize];
        stream.read_with_correction(&mut out1, 50, 0);

        stream.add_chunk(make_chunk(100, 0, 100, f));
        stream.current = stream.chunks.pop_front();

        let mut out2 = vec![0u8; 50 * f.frame_size() as usize];
        stream.read_next(&mut out2, 50);

        assert_eq!(out1, out2);
    }
}
