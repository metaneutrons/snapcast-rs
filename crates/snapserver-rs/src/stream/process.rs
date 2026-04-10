//! Process stream reader — captures stdout PCM from a child process.

use anyhow::Result;
use snapcast_proto::SampleFormat;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::uri::StreamUri;
use snapcast_server::stream::PcmChunk;
use snapcast_server::time::ChunkTimestamper;

/// Start a child process and read PCM from its stdout.
pub fn start(
    uri: StreamUri,
    format: SampleFormat,
    chunk_frames: usize,
    tx: mpsc::Sender<PcmChunk>,
) -> Result<JoinHandle<()>> {
    let path = uri.path.clone();
    let params = uri.param("params").unwrap_or("").to_string();
    let chunk_bytes = chunk_frames * format.frame_size() as usize;

    Ok(tokio::spawn(async move {
        let mut ts = ChunkTimestamper::new(format.rate());
        loop {
            tracing::info!(path, params, "Starting process stream");
            let args: Vec<&str> = if params.is_empty() {
                vec![]
            } else {
                params.split_whitespace().collect()
            };

            let Ok(mut child) = Command::new(&path)
                .args(&args)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .spawn()
            else {
                tracing::error!(path, "Failed to start process");
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                continue;
            };

            let Some(mut stdout) = child.stdout.take() else {
                let _ = child.kill().await;
                continue;
            };

            let mut buf = vec![0u8; chunk_bytes];
            while stdout.read_exact(&mut buf).await.is_ok() {
                let chunk = PcmChunk {
                    timestamp_usec: ts.next(chunk_frames as u32),
                    data: buf.clone(),
                };
                if tx.send(chunk).await.is_err() {
                    let _ = child.kill().await;
                    return;
                }
            }

            let _ = child.kill().await;
            tracing::info!(path, "Process exited, restarting");
            ts.reset();
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    }))
}
