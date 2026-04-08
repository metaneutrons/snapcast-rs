//! Stream readers — async PCM audio sources.

pub mod file;
pub mod manager;
pub mod pipe;
pub mod process;
pub mod tcp;
pub mod uri;

use anyhow::Result;
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

/// Common properties parsed from a stream URI.
pub struct StreamProps {
    /// Stream name (from `?name=` parameter).
    pub name: String,
    /// Sample format (from `?sampleformat=` or default).
    pub format: SampleFormat,
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

/// Create and start a stream reader from a URI string.
pub fn create(source_uri: &str, default_format: SampleFormat) -> Result<StreamReader> {
    let parsed = uri::StreamUri::parse(source_uri)?;
    let name = parsed.param("name").unwrap_or("default").to_string();
    let format = parsed
        .param("sampleformat")
        .and_then(|s| s.parse().ok())
        .unwrap_or(default_format);

    let (tx, rx) = mpsc::channel(128);

    let handle = match parsed.scheme.as_str() {
        "pipe" => pipe::start(parsed, format, tx)?,
        "file" => file::start(parsed, format, tx)?,
        "process" => process::start(parsed, format, tx)?,
        "tcp" => tcp::start(parsed, format, tx)?,
        other => anyhow::bail!("unsupported stream scheme: {other}"),
    };

    Ok(StreamReader {
        name,
        format,
        rx,
        handle,
    })
}
