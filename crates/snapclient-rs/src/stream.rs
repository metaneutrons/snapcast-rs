//! Time-synchronized PCM audio stream buffer.

use std::collections::VecDeque;

use snapcast_proto::SampleFormat;
use snapcast_proto::types::Timeval;

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

    pub fn start_usec(&self) -> i64 {
        self.timestamp.sec as i64 * 1_000_000 + self.timestamp.usec as i64
    }

    pub fn duration_usec(&self) -> i64 {
        if self.format.frame_size() == 0 || self.format.rate() == 0 {
            return 0;
        }
        let frames = self.data.len() as i64 / self.format.frame_size() as i64;
        frames * 1_000_000 / self.format.rate() as i64
    }

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

    pub fn is_end(&self) -> bool {
        self.read_pos >= self.data.len()
    }

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
    hard_sync: bool,
}

impl Stream {
    pub fn new(format: SampleFormat) -> Self {
        Self {
            format,
            chunks: VecDeque::new(),
            current: None,
            buffer_ms: 1000,
            hard_sync: true,
        }
    }

    pub fn format(&self) -> SampleFormat {
        self.format
    }

    pub fn set_buffer_ms(&mut self, ms: i64) {
        self.buffer_ms = ms;
    }

    pub fn add_chunk(&mut self, chunk: PcmChunk) {
        self.chunks.push_back(chunk);
    }

    pub fn chunk_count(&self) -> usize {
        self.chunks.len()
    }

    pub fn clear(&mut self) {
        self.chunks.clear();
        self.current = None;
        self.hard_sync = true;
    }

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

        if self.hard_sync {
            let chunk = self.current.as_ref().unwrap();
            let req_duration_usec = (frames as i64 * 1_000_000) / self.format.rate() as i64;
            let age_usec = server_now_usec - chunk.start_usec() - self.buffer_ms * 1000
                + output_buffer_dac_time_usec;

            if age_usec < -req_duration_usec {
                // Too early — play silence
                self.get_silence(output, frames);
                return true;
            }

            if age_usec > 0 {
                // Too late — drop old chunks until we catch up
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

            // Recompute age after potential chunk changes
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
                }
                return true;
            }
            return false;
        }

        // Normal playback — just read frames sequentially
        self.read_next(output, frames);
        true
    }

    pub fn get_silence(&self, output: &mut [u8], frames: u32) {
        let bytes = frames as usize * self.format.frame_size() as usize;
        output[..bytes].fill(0);
    }

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
}
