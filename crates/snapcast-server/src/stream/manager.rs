//! Stream manager — owns stream readers, encodes PCM, distributes to clients.

use std::collections::HashMap;

use anyhow::Result;
use snapcast_proto::SampleFormat;
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;

use crate::encoder::{self, Encoder};
use crate::stream::{self, StreamReader};

/// An encoded chunk ready to be sent to clients as a WireChunk.
#[derive(Debug, Clone)]
pub struct WireChunkData {
    /// Stream ID this chunk belongs to.
    pub stream_id: String,
    /// Server timestamp in microseconds.
    pub timestamp_usec: i64,
    /// Encoded audio data.
    pub data: Vec<u8>,
}

/// Info about a managed stream.
struct ManagedStream {
    format: SampleFormat,
    header: Vec<u8>,
    codec: String,
    _reader: StreamReader,
    _encode_handle: JoinHandle<()>,
}

/// Manages all audio streams, encoding, and distribution.
pub struct StreamManager {
    streams: HashMap<String, ManagedStream>,
    /// Broadcast sender for encoded chunks — client sessions subscribe.
    chunk_tx: broadcast::Sender<WireChunkData>,
}

impl Default for StreamManager {
    fn default() -> Self {
        Self::new()
    }
}

impl StreamManager {
    /// Create a new stream manager.
    pub fn new() -> Self {
        let (chunk_tx, _) = broadcast::channel(256);
        Self {
            streams: HashMap::new(),
            chunk_tx,
        }
    }

    /// Get the broadcast sender (for passing to SessionServer).
    pub fn chunk_sender(&self) -> broadcast::Sender<WireChunkData> {
        self.chunk_tx.clone()
    }

    /// Get a broadcast receiver for encoded chunks.
    pub fn subscribe(&self) -> broadcast::Receiver<WireChunkData> {
        self.chunk_tx.subscribe()
    }

    /// Add a stream from a source URI. Starts reading and encoding immediately.
    pub fn add_stream(
        &mut self,
        source_uri: &str,
        default_format: SampleFormat,
        codec: &str,
        codec_options: &str,
    ) -> Result<()> {
        let mut reader = stream::create(source_uri, default_format)?;
        let name = reader.name.clone();
        let format = reader.format;

        // Create encoder to capture header
        let enc = encoder::create(codec, format, codec_options)?;
        let header = enc.header().to_vec();
        let codec_name = enc.name().to_string();
        drop(enc); // Don't move across threads — recreate on encode thread

        let stream_id = name.clone();
        let chunk_tx = self.chunk_tx.clone();
        let codec_str = codec.to_string();
        let opts_str = codec_options.to_string();

        // Take the receiver from the reader
        let reader_rx = std::mem::replace(&mut reader.rx, mpsc::channel(1).1);

        // Encoder may be !Send (FLAC/libflac), so create + run on a dedicated OS thread
        let encode_handle = {
            let (done_tx, done_rx) = tokio::sync::oneshot::channel::<()>();
            std::thread::spawn(move || {
                let Ok(enc) = encoder::create(&codec_str, format, &opts_str) else {
                    return;
                };
                encode_loop(enc, reader_rx, &chunk_tx, &stream_id);
                let _ = done_tx.send(());
            });
            tokio::spawn(async move {
                let _ = done_rx.await;
            })
        };

        self.streams.insert(
            name.clone(),
            ManagedStream {
                format,
                header,
                codec: codec_name.clone(),
                _reader: reader,
                _encode_handle: encode_handle,
            },
        );

        tracing::info!(name, %format, codec = codec_name, "Stream added");
        Ok(())
    }

    /// Get codec header for a stream: (codec_name, header_bytes, format).
    pub fn header(&self, stream_id: &str) -> Option<(&str, &[u8], SampleFormat)> {
        self.streams
            .get(stream_id)
            .map(|s| (s.codec.as_str(), s.header.as_slice(), s.format))
    }

    /// List all stream IDs.
    pub fn stream_ids(&self) -> Vec<String> {
        self.streams.keys().cloned().collect()
    }
}

/// Blocking encode loop — runs on a dedicated thread via spawn_blocking.
fn encode_loop(
    mut enc: Box<dyn Encoder>,
    mut rx: mpsc::Receiver<stream::PcmChunk>,
    tx: &broadcast::Sender<WireChunkData>,
    stream_id: &str,
) {
    // Track timestamp of first buffered chunk (for codecs that buffer internally)
    let mut pending_timestamp: Option<i64> = None;

    while let Some(pcm) = rx.blocking_recv() {
        // Save timestamp of first chunk fed to encoder
        if pending_timestamp.is_none() {
            pending_timestamp = Some(pcm.timestamp_usec);
        }

        match enc.encode(&pcm.data) {
            Ok(encoded) => {
                if encoded.data.is_empty() {
                    // Encoder is buffering — keep the pending timestamp
                    continue;
                }
                let wire = WireChunkData {
                    stream_id: stream_id.to_string(),
                    timestamp_usec: pending_timestamp.take().unwrap_or(pcm.timestamp_usec),
                    data: encoded.data,
                };
                let _ = tx.send(wire);
                // Reset for next frame
                pending_timestamp = None;
            }
            Err(e) => {
                tracing::warn!(stream_id, error = %e, "Encode failed");
                pending_timestamp = None;
            }
        }
    }
}
