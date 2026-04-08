//! Pipe stream reader — reads PCM from a named pipe (FIFO).

use anyhow::Result;
use snapcast_proto::SampleFormat;
use tokio::io::AsyncReadExt;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::PcmChunk;
use super::uri::StreamUri;
use crate::time::ChunkTimestamper;

/// Start reading PCM from a named pipe.
pub fn start(
    uri: StreamUri,
    format: SampleFormat,
    tx: mpsc::Sender<PcmChunk>,
) -> Result<JoinHandle<()>> {
    let path = uri.path.clone();
    let chunk_ms = 20;
    let chunk_frames = (format.rate() as usize * chunk_ms) / 1000;
    let chunk_bytes = chunk_frames * format.frame_size() as usize;

    Ok(tokio::spawn(async move {
        loop {
            match tokio::fs::OpenOptions::new().read(true).open(&path).await {
                Ok(mut file) => {
                    tracing::info!(path, "Pipe stream opened");
                    let mut ts = ChunkTimestamper::new(format.rate());
                    let mut buf = vec![0u8; chunk_bytes];
                    loop {
                        match file.read_exact(&mut buf).await {
                            Ok(_) => {
                                let chunk = PcmChunk {
                                    timestamp_usec: ts.next(chunk_frames as u32),
                                    data: buf.clone(),
                                };
                                if tx.send(chunk).await.is_err() {
                                    return;
                                }
                            }
                            Err(e) => {
                                tracing::debug!(error = %e, "Pipe read error, reopening");
                                break;
                            }
                        }
                    }
                }
                Err(e) => {
                    tracing::debug!(path, error = %e, "Pipe not available, retrying");
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    }))
}
