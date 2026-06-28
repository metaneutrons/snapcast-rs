//! File stream reader — reads PCM/WAV from a file, loops on EOF.

use anyhow::Result;
use snapcast_proto::SampleFormat;
use snapcast_server::AudioFrame;
use snapcast_server::time::ChunkTimestamper;
use tokio::io::AsyncReadExt;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::uri::StreamUri;
use super::{PumpEnd, pump_pcm};

/// Start reading PCM from a file, looping on EOF.
pub fn start(
    uri: StreamUri,
    format: SampleFormat,
    chunk_frames: usize,
    tx: mpsc::Sender<AudioFrame>,
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

                    // Paced reads so a finite file plays back in real time.
                    match pump_pcm(
                        &mut file,
                        &mut ts,
                        chunk_frames,
                        chunk_bytes,
                        &tx,
                        Some(chunk_duration),
                    )
                    .await
                    {
                        PumpEnd::SourceEnded => {} // EOF → reopen and loop
                        PumpEnd::TxClosed => return,
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
