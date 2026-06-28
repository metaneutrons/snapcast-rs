//! Time utilities — server timestamp generation using the monotonic clock.
//!
//! The clock itself is the single source of truth in [`snapcast_proto::time`]
//! so the server and client cannot drift onto different clock domains.

/// Current monotonic time in microseconds.
pub fn now_usec() -> i64 {
    snapcast_proto::time::now_usec()
}

/// Generates evenly-spaced timestamps based on sample count.
/// This matches the C++ server behavior: timestamps reflect when audio
/// *should* be played, not when it was read from the source.
pub struct ChunkTimestamper {
    start_usec: i64,
    samples_written: u64,
    rate: u32,
}

impl ChunkTimestamper {
    /// Create a new timestamper anchored at the current time.
    pub fn new(rate: u32) -> Self {
        Self {
            start_usec: now_usec(),
            samples_written: 0,
            rate,
        }
    }

    /// Get the timestamp for the next chunk of `frames` frames.
    pub fn next(&mut self, frames: u32) -> i64 {
        let ts = self.start_usec + (self.samples_written as i64 * 1_000_000) / self.rate as i64;
        self.samples_written += frames as u64;
        ts
    }

    /// Reset the timestamper (e.g. on stream restart).
    pub fn reset(&mut self) {
        self.start_usec = now_usec();
        self.samples_written = 0;
    }
}
