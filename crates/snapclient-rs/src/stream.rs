//! Time-synchronized PCM audio stream buffer.
//!
//! Port of the C++ `Stream` class. Manages a queue of decoded PCM chunks,
//! aligns playback to server time, and corrects drift by inserting/removing
//! individual samples.

use std::collections::VecDeque;

use snapcast_proto::SampleFormat;
use snapcast_proto::types::Timeval;

use crate::double_buffer::DoubleBuffer;

/// A decoded PCM chunk with a server-time timestamp and a read cursor.
#[derive(Debug, Clone)]
pub struct PcmChunk {
    pub timestamp: Timeval,
    pub data: Vec<u8>,
    pub format: SampleFormat,
    read_pos: usize,
}

impl PcmChunk {
    pub fn new(timestamp: Timeval, data: Vec<u8>, format: SampleFormat) -> Self {
        Self {
            timestamp,
            data,
            format,
            read_pos: 0,
        }
    }

    /// Start time as microseconds since epoch.
    pub fn start_usec(&self) -> i64 {
        self.timestamp.sec as i64 * 1_000_000 + self.timestamp.usec as i64
    }

    /// Duration in microseconds based on payload size and format.
    pub fn duration_usec(&self) -> i64 {
        if self.format.frame_size() == 0 || self.format.rate() == 0 {
            return 0;
        }
        let frames = self.data.len() as i64 / self.format.frame_size() as i64;
        frames * 1_000_000 / self.format.rate() as i64
    }

    /// Read up to `frames` frames from the chunk. Returns number of frames actually read.
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

    /// Whether all data has been consumed.
    pub fn is_end(&self) -> bool {
        self.read_pos >= self.data.len()
    }

    /// Skip `frames` frames.
    pub fn seek(&mut self, frames: u32) {
        let bytes = frames as usize * self.format.frame_size() as usize;
        self.read_pos = (self.read_pos + bytes).min(self.data.len());
    }
}

/// Time-synchronized PCM stream buffer.
pub struct Stream {
    format: SampleFormat,
    chunks: VecDeque<PcmChunk>,
    current: Option<PcmChunk>,
    buffer_ms: i64,

    // Drift detection buffers
    mini_buffer: DoubleBuffer,
    short_buffer: DoubleBuffer,
    buffer: DoubleBuffer,

    median: i64,
    short_median: i64,
    hard_sync: bool,
    played_frames: u32,
    correct_after_x_frames: i32,
    frame_delta: i32,
    read_buf: Vec<u8>,
}

impl Stream {
    pub fn new(format: SampleFormat) -> Self {
        Self {
            format,
            chunks: VecDeque::new(),
            current: None,
            buffer_ms: 1000,
            mini_buffer: DoubleBuffer::new(20),
            short_buffer: DoubleBuffer::new(100),
            buffer: DoubleBuffer::new(500),
            median: 0,
            short_median: 0,
            hard_sync: true,
            played_frames: 0,
            correct_after_x_frames: 0,
            frame_delta: 0,
            read_buf: Vec::new(),
        }
    }

    pub fn format(&self) -> SampleFormat {
        self.format
    }

    pub fn set_buffer_ms(&mut self, ms: i64) {
        self.buffer_ms = ms;
    }

    /// Add a decoded PCM chunk to the queue.
    pub fn add_chunk(&mut self, chunk: PcmChunk) {
        self.chunks.push_back(chunk);
    }

    /// Number of chunks in the queue.
    pub fn chunk_count(&self) -> usize {
        self.chunks.len()
    }

    /// Clear all chunks and reset sync state.
    pub fn clear(&mut self) {
        self.chunks.clear();
        self.current = None;
        self.reset_buffers();
    }

    /// Get the next player chunk, time-aligned to server time.
    ///
    /// `server_now_usec`: current server time in microseconds
    /// `output_buffer_dac_time_usec`: time until the buffer reaches the DAC
    /// `output`: buffer to fill with PCM data
    /// `frames`: number of frames requested
    ///
    /// Returns `true` if audio data was written, `false` if silence should be played.
    pub fn get_player_chunk(
        &mut self,
        server_now_usec: i64,
        output_buffer_dac_time_usec: i64,
        output: &mut [u8],
        frames: u32,
    ) -> bool {
        if output_buffer_dac_time_usec > self.buffer_ms * 1000 {
            return false;
        }

        // Ensure we have a current chunk
        let needs_new = self.current.as_ref().is_none_or(|c| c.is_end());
        if needs_new {
            self.current = self.chunks.pop_front();
        }
        if self.current.is_none() {
            return false;
        }

        let frame_size = self.format.frame_size() as usize;
        let req_bytes = frames as usize * frame_size;

        if self.hard_sync {
            return self.do_hard_sync(server_now_usec, output_buffer_dac_time_usec, output, frames);
        }

        // Soft sync: compute sample rate correction
        let mut frames_correction: i32 = 0;
        if self.correct_after_x_frames != 0 {
            self.played_frames += frames;
            if self.played_frames >= self.correct_after_x_frames.unsigned_abs() {
                frames_correction = self.played_frames as i32 / self.correct_after_x_frames;
                self.played_frames %= self.correct_after_x_frames.unsigned_abs();
            }
        }

        // Read frames with correction
        let chunk_start = match self.read_with_correction(output, frames, frames_correction) {
            Some(ts) => ts,
            None => return false,
        };

        let age_usec =
            server_now_usec - chunk_start - self.buffer_ms * 1000 + output_buffer_dac_time_usec;

        self.set_real_sample_rate(self.format.rate() as f64);

        // Check if hard sync is needed
        let needs_hard_sync = (self.buffer.full()
            && self.median.abs() > 2000
            && age_usec.abs() > 500)
            || (self.short_buffer.full() && self.short_median.abs() > 5000 && age_usec.abs() > 500)
            || (self.mini_buffer.full()
                && self.mini_buffer.median_simple().abs() > 50000
                && age_usec.abs() > 500)
            || age_usec.abs() > 500_000;

        if needs_hard_sync {
            self.hard_sync = true;
        } else if self.short_buffer.full() {
            // Soft sync: adjust playback speed
            let mini_median = self.mini_buffer.median_simple();
            if self.short_median > 100 && mini_median > 50 && age_usec > 50 {
                let rate_adj = (self.short_median as f64 / 100.0) * 0.00005;
                let rate = 1.0 - rate_adj.min(0.0005);
                self.set_real_sample_rate(self.format.rate() as f64 * rate);
            } else if self.short_median < -100 && mini_median < -50 && age_usec < -50 {
                let rate_adj = (-self.short_median as f64 / 100.0) * 0.00005;
                let rate = 1.0 + rate_adj.min(0.0005);
                self.set_real_sample_rate(self.format.rate() as f64 * rate);
            }
        }

        self.update_buffers(age_usec);
        self.median = self.buffer.median_simple();
        self.short_median = self.short_buffer.median_simple();

        let _ = req_bytes; // used implicitly via frames
        age_usec.abs() < 500_000
    }

    /// Fill output with silence.
    pub fn get_silence(&self, output: &mut [u8], frames: u32) {
        let bytes = frames as usize * self.format.frame_size() as usize;
        output[..bytes].fill(0);
    }

    /// Get player chunk, falling back to silence on failure.
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

    // --- Private helpers ---

    fn do_hard_sync(
        &mut self,
        server_now_usec: i64,
        dac_time_usec: i64,
        output: &mut [u8],
        frames: u32,
    ) -> bool {
        let Some(chunk) = self.current.as_ref() else {
            return false;
        };
        let frame_ns = 1_000_000_000.0 / self.format.rate() as f64;
        let req_duration_usec = (frames as f64 * frame_ns / 1000.0) as i64;

        let age_usec = server_now_usec - chunk.start_usec() - self.buffer_ms * 1000 + dac_time_usec;

        if age_usec < -req_duration_usec {
            // Too early — play silence
            self.get_silence(output, frames);
            return true;
        }

        if age_usec > 0 {
            // Too late — drop old chunks until we catch up
            self.current = None;
            while let Some(mut c) = self.chunks.pop_front() {
                let a = server_now_usec - c.start_usec() - self.buffer_ms * 1000 + dac_time_usec;
                if a > 0 && a < c.duration_usec() {
                    // Fast-forward within this chunk
                    let skip_frames = (self.format.rate() as f64 * a as f64 / 1_000_000.0) as u32;
                    c.seek(skip_frames);
                    self.current = Some(c);
                    break;
                } else if a <= 0 {
                    self.current = Some(c);
                    break;
                }
                // else: chunk entirely too old, drop it
            }
            if self.current.is_none() {
                return false;
            }
        }

        // Play with partial silence if needed
        let Some(chunk) = self.current.as_ref() else {
            return false;
        };
        let age_usec = server_now_usec - chunk.start_usec() - self.buffer_ms * 1000 + dac_time_usec;

        if age_usec <= 0 {
            let silent_frames =
                (self.format.rate() as f64 * (-age_usec) as f64 / 1_000_000.0) as u32;
            let silent_frames = silent_frames.min(frames);
            if silent_frames > 0 {
                self.get_silence(
                    &mut output[..silent_frames as usize * self.format.frame_size() as usize],
                    silent_frames,
                );
            }
            let remaining = frames - silent_frames;
            if remaining > 0 {
                let offset = silent_frames as usize * self.format.frame_size() as usize;
                self.read_next(&mut output[offset..], remaining);
            }
            self.hard_sync = false;
            self.reset_buffers();
            return true;
        }

        false
    }

    fn read_next(&mut self, output: &mut [u8], frames: u32) -> Option<i64> {
        let chunk = self.current.as_mut()?;
        let ts = chunk.start_usec();
        let mut read = 0u32;
        while read < frames {
            let frame_size = self.format.frame_size() as usize;
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

        self.frame_delta -= correction;
        let to_read = (frames as i32 + correction) as u32;
        let frame_size = self.format.frame_size() as usize;

        // Take read_buf out of self to avoid double borrow
        let mut read_buf = std::mem::take(&mut self.read_buf);
        read_buf.resize(to_read as usize * frame_size, 0);
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
                let src_offset = (pos - n) * frame_size;
                let dst_offset = pos * frame_size;
                output[dst_offset..dst_offset + size * frame_size]
                    .copy_from_slice(&read_buf[src_offset..src_offset + size * frame_size]);
            } else {
                let src_offset = pos * frame_size;
                let dst_offset = (pos - n) * frame_size;
                output[dst_offset..dst_offset + size * frame_size]
                    .copy_from_slice(&read_buf[src_offset..src_offset + size * frame_size]);
            }
            pos += size;
        }

        // Put read_buf back
        self.read_buf = read_buf;
        ts
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

    fn update_buffers(&mut self, age_usec: i64) {
        self.buffer.add(age_usec);
        self.mini_buffer.add(age_usec);
        self.short_buffer.add(age_usec);
    }

    fn reset_buffers(&mut self) {
        self.buffer.clear();
        self.mini_buffer.clear();
        self.short_buffer.clear();
        self.hard_sync = true;
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
        // Fill with non-zero pattern so we can detect silence vs data
        let data: Vec<u8> = (0..bytes).map(|i| (i % 256) as u8).collect();
        PcmChunk::new(Timeval { sec, usec }, data, format)
    }

    #[test]
    fn pcm_chunk_duration() {
        let f = fmt();
        let chunk = make_chunk(0, 0, 480, f); // 480 frames @ 48kHz = 10ms
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
        assert_eq!(read, 10); // only 10 frames left
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

        // Chunk at t=100s, server_now=100s → age = 0 - 1000ms = -1000ms (too early)
        stream.add_chunk(make_chunk(100, 0, 4800, f));
        let server_now = 100_000_000i64; // 100s in usec
        let mut buf = vec![0xFFu8; 480 * f.frame_size() as usize];
        let result = stream.get_player_chunk(server_now, 0, &mut buf, 480);
        // Should play silence (chunk is 1000ms in the future)
        assert!(result);
        assert!(buf.iter().all(|&b| b == 0)); // silence
    }

    #[test]
    fn stream_hard_sync_plays_data_when_aligned() {
        let f = fmt();
        let mut stream = Stream::new(f);
        stream.set_buffer_ms(1000);

        // Chunk at t=99s, server_now=100s → age = 1s - 1000ms = 0 (perfect)
        stream.add_chunk(make_chunk(99, 0, 4800, f));
        let server_now = 100_000_000i64;
        let mut buf = vec![0u8; 480 * f.frame_size() as usize];
        let result = stream.get_player_chunk(server_now, 0, &mut buf, 480);
        assert!(result);
        // Should have non-zero data (our test chunks have patterned data)
        assert!(buf.iter().any(|&b| b != 0));
    }

    #[test]
    fn set_real_sample_rate_correction() {
        let f = fmt();
        let mut stream = Stream::new(f);
        stream.set_real_sample_rate(48000.0);
        assert_eq!(stream.correct_after_x_frames, 0);

        // Slightly fast → need to insert frames
        stream.set_real_sample_rate(47999.0);
        assert_ne!(stream.correct_after_x_frames, 0);
    }
}
