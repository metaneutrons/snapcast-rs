//! Pipe stream reader — reads PCM from a named pipe (FIFO).

use anyhow::Result;
use snapcast_proto::SampleFormat;
use snapcast_server::{AudioData, AudioFrame};
use tokio::io::AsyncReadExt;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::uri::StreamUri;
use snapcast_server::time::ChunkTimestamper;

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
                    let mut buf = vec![0u8; chunk_bytes];
                    loop {
                        match file.read_exact(&mut buf).await {
                            Ok(_) => {
                                let frame = AudioFrame {
                                    data: AudioData::Pcm(buf.clone()),
                                    timestamp_usec: ts.next(chunk_frames as u32),
                                };
                                if tx.send(frame).await.is_err() {
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
