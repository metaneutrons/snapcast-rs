//! Stream management — encoding and distribution.

pub mod manager;

use snapcast_proto::SampleFormat;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

/// A timestamped PCM chunk produced by a stream reader.
#[derive(Debug, Clone)]
pub struct PcmChunk {
    /// Server timestamp in microseconds when this chunk was captured.
    pub timestamp_usec: i64,
    /// Raw PCM audio data.
    pub data: Vec<u8>,
}

/// A running stream reader that produces PCM chunks.
pub struct StreamReader {
    /// Stream name.
    pub name: String,
    /// Sample format.
    pub format: SampleFormat,
    /// Receiver for PCM chunks.
    pub rx: mpsc::Receiver<PcmChunk>,
    /// Task handle for the reader.
    handle: JoinHandle<()>,
}

impl StreamReader {
    /// Create a StreamReader from components.
    pub fn new(
        name: String,
        format: SampleFormat,
        rx: mpsc::Receiver<PcmChunk>,
        handle: JoinHandle<()>,
    ) -> Self {
        Self {
            name,
            format,
            rx,
            handle,
        }
    }

    /// Stop the reader.
    pub fn stop(&self) {
        self.handle.abort();
    }
}

impl Drop for StreamReader {
    fn drop(&mut self) {
        self.handle.abort();
    }
}
