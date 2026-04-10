//! File stream reader — reads PCM/WAV from a file, loops on EOF.

use anyhow::Result;
use snapcast_proto::SampleFormat;
use tokio::io::AsyncReadExt;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::uri::StreamUri;
use snapcast_server::stream::PcmChunk;
use snapcast_server::time::ChunkTimestamper;

/// Start reading PCM from a file, looping on EOF.
pub fn start(
    uri: StreamUri,
    format: SampleFormat,
    chunk_frames: usize,
    tx: mpsc::Sender<PcmChunk>,
) -> Result<JoinHandle<()>> {
    let path = uri.path.clone();
    let chunk_bytes = chunk_frames * format.frame_size() as usize;
    let chunk_duration =
        std::time::Duration::from_micros((chunk_frames as u64 * 1_000_000) / format.rate() as u64);

    Ok(tokio::spawn(async move {
        let mut ts = ChunkTimestamper::new(format.rate());
        loop {
            match tokio::fs::File::open(&path).await {
                Ok(mut file) => {
                    tracing::info!(path, "File stream opened");
                    // Skip WAV header if present
                    let mut header = [0u8; 4];
                    if file.read_exact(&mut header).await.is_ok() && &header == b"RIFF" {
                        // Skip remaining 40 bytes of WAV header
                        let mut skip = [0u8; 40];
                        let _ = file.read_exact(&mut skip).await;
                    }

                    let mut buf = vec![0u8; chunk_bytes];
                    let mut interval = tokio::time::interval(chunk_duration);
                    loop {
                        interval.tick().await;
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
                            Err(_) => break, // EOF → reopen and loop
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(path, error = %e, "File not found, retrying");
                }
            }
            ts.reset();
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    }))
}
