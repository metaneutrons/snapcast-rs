//! Librespot stream reader — spawns librespot, parses metadata from stderr.

use anyhow::Result;
use snapcast_proto::SampleFormat;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::PcmChunk;
use super::uri::StreamUri;
use crate::time::now_usec;

/// Start librespot and read PCM from stdout, metadata from stderr.
pub fn start(
    uri: StreamUri,
    format: SampleFormat,
    tx: mpsc::Sender<PcmChunk>,
    meta_tx: mpsc::Sender<(String, String)>,
) -> Result<JoinHandle<()>> {
    let devicename = uri.param("devicename").unwrap_or("Snapcast").to_string();
    let bitrate = uri.param("bitrate").unwrap_or("320").to_string();
    let username = uri.param("username").unwrap_or("").to_string();
    let password = uri.param("password").unwrap_or("").to_string();
    let cache = uri.param("cache").unwrap_or("").to_string();
    let volume = uri.param("volume").unwrap_or("100").to_string();
    let normalize = uri.param("normalize").unwrap_or("false") == "true";
    let autoplay = uri.param("autoplay").unwrap_or("false") == "true";

    let mut args = vec![
        "--name".into(),
        devicename,
        "--bitrate".into(),
        bitrate,
        "--backend".into(),
        "pipe".into(),
        "--initial-volume".into(),
        volume,
        "--verbose".into(),
    ];
    if !username.is_empty() && !password.is_empty() {
        args.extend(["--username".into(), username, "--password".into(), password]);
    }
    if !cache.is_empty() {
        args.extend(["--cache".into(), cache]);
    }
    if normalize {
        args.push("--enable-volume-normalisation".into());
    }
    if autoplay {
        args.extend(["--autoplay".into(), "on".into()]);
    }

    let chunk_bytes = (format.rate() as usize * format.frame_size() as usize * 20) / 1000;

    Ok(tokio::spawn(async move {
        loop {
            tracing::info!(args = ?args, "Starting librespot");
            let Ok(mut child) = Command::new("librespot")
                .args(&args)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
            else {
                tracing::error!("Failed to start librespot");
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                continue;
            };

            let Some(stdout) = child.stdout.take() else {
                let _ = child.kill().await;
                continue;
            };
            let stderr = child.stderr.take();

            // Spawn stderr metadata parser
            let meta_tx2 = meta_tx.clone();
            if let Some(stderr) = stderr {
                tokio::spawn(async move {
                    parse_librespot_stderr(stderr, meta_tx2).await;
                });
            }

            // Read PCM from stdout
            let mut reader = tokio::io::BufReader::new(stdout);
            let mut buf = vec![0u8; chunk_bytes];
            while reader.read_exact(&mut buf).await.is_ok() {
                let chunk = PcmChunk {
                    timestamp_usec: now_usec(),
                    data: buf.clone(),
                };
                if tx.send(chunk).await.is_err() {
                    let _ = child.kill().await;
                    return;
                }
            }

            let _ = child.kill().await;
            tracing::info!("librespot exited, restarting");
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
    }))
}

/// Parse librespot stderr for track metadata.
/// Format: `[...INFO  librespot_playback::player] <Track Name> (123456 ms) loaded`
async fn parse_librespot_stderr<R: AsyncReadExt + Unpin>(
    stderr: R,
    meta_tx: mpsc::Sender<(String, String)>,
) {
    let mut lines = BufReader::new(stderr).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        // Parse: <Title> (duration_ms ms) loaded
        if let Some(start) = line.find('<')
            && let Some(end) = line.find('>')
            && line.contains("ms) loaded")
            && start < end
        {
            let title = &line[start + 1..end];
            tracing::info!(title, "Librespot: track loaded");
            let _ = meta_tx.send(("title".into(), title.to_string())).await;
        }
        // Forward log lines at appropriate level
        if line.contains("ERROR") {
            tracing::warn!(target: "librespot", "{line}");
        } else if line.contains("WARN") {
            tracing::debug!(target: "librespot", "{line}");
        }
    }
}
