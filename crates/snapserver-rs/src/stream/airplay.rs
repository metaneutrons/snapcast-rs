//! Airplay stream reader — spawns shairport-sync, parses metadata from pipe.

use anyhow::Result;
use snapcast_proto::SampleFormat;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::uri::StreamUri;
use snapcast_server::time::ChunkTimestamper;
use snapcast_server::{AudioData, AudioFrame};

/// Start shairport-sync and read PCM from stdout.
pub fn start(
    uri: StreamUri,
    format: SampleFormat,
    tx: mpsc::Sender<AudioFrame>,
    meta_tx: mpsc::Sender<(String, String)>,
) -> Result<JoinHandle<()>> {
    let devicename = uri.param("devicename").unwrap_or("Snapcast").to_string();
    let port = uri.param("port").unwrap_or("5000").to_string();
    let password = uri.param("password").unwrap_or("").to_string();

    let mut args: Vec<String> = vec![
        format!("--name={devicename}"),
        "--output=stdout".into(),
        "--get-coverart".into(),
        format!("--port={port}"),
    ];
    if !password.is_empty() {
        args.push(format!("--password={password}"));
    }

    let chunk_bytes = (format.rate() as usize * format.frame_size() as usize * 20) / 1000;
    let chunk_frames = chunk_bytes / format.frame_size() as usize;

    Ok(tokio::spawn(async move {
        let mut ts = ChunkTimestamper::new(format.rate());
        loop {
            tracing::info!(args = ?args, "Starting shairport-sync");
            let Ok(mut child) = Command::new("shairport-sync")
                .args(&args)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
            else {
                tracing::error!("Failed to start shairport-sync");
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                continue;
            };

            let Some(mut stdout) = child.stdout.take() else {
                let _ = child.kill().await;
                continue;
            };

            // Parse stderr for metadata (shairport-sync logs track info)
            if let Some(stderr) = child.stderr.take() {
                let meta_tx2 = meta_tx.clone();
                tokio::spawn(async move {
                    parse_airplay_stderr(stderr, meta_tx2).await;
                });
            }

            let mut buf = vec![0u8; chunk_bytes];
            while stdout.read_exact(&mut buf).await.is_ok() {
                let frame = AudioFrame {
                    timestamp_usec: ts.next(chunk_frames as u32),
                    data: AudioData::Pcm(buf.clone()),
                };
                if tx.send(frame).await.is_err() {
                    let _ = child.kill().await;
                    return;
                }
            }

            let _ = child.kill().await;
            tracing::info!("shairport-sync exited, restarting");
            ts.reset();
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
    }))
}

/// Parse shairport-sync stderr for metadata.
async fn parse_airplay_stderr<R: AsyncReadExt + Unpin>(
    stderr: R,
    meta_tx: mpsc::Sender<(String, String)>,
) {
    let mut lines = tokio::io::BufReader::new(stderr).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        // shairport-sync metadata format varies by version
        // Common: "Title: ...", "Artist: ...", "Album: ..."
        if let Some(title) = line.strip_prefix("Title: ") {
            tracing::info!(title, "Airplay: track");
            let _ = meta_tx.send(("title".into(), title.to_string())).await;
        } else if let Some(artist) = line.strip_prefix("Artist: ") {
            let _ = meta_tx.send(("artist".into(), artist.to_string())).await;
        } else if let Some(album) = line.strip_prefix("Album: ") {
            let _ = meta_tx.send(("album".into(), album.to_string())).await;
        }
    }
}

use tokio::io::AsyncBufReadExt;
