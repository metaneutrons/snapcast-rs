//! Pipe stream reader — reads PCM from a named pipe (FIFO).

use anyhow::Result;
use snapcast_proto::SampleFormat;
use snapcast_server::AudioFrame;
use snapcast_server::time::ChunkTimestamper;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::uri::StreamUri;
use super::{PumpEnd, pump_pcm};

/// Start reading PCM from a named pipe.
pub fn start(
    uri: StreamUri,
    format: SampleFormat,
    chunk_frames: usize,
    tx: mpsc::Sender<AudioFrame>,
) -> Result<JoinHandle<()>> {
    let path = uri.path.clone();
    let chunk_bytes = chunk_frames * format.frame_size() as usize;

    Ok(tokio::spawn(async move {
        loop {
            match tokio::fs::OpenOptions::new().read(true).open(&path).await {
                Ok(mut file) => {
                    tracing::info!(path, "Pipe stream opened");
                    let mut ts = ChunkTimestamper::new(format.rate());
                    match pump_pcm(&mut file, &mut ts, chunk_frames, chunk_bytes, &tx, None).await {
                        PumpEnd::SourceEnded => {
                            tracing::debug!(path, "Pipe read ended, reopening");
                        }
                        PumpEnd::TxClosed => return,
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
