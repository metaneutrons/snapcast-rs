//! TCP stream reader — accepts a TCP connection and reads PCM from it.

use anyhow::Result;
use snapcast_proto::SampleFormat;
use tokio::io::AsyncReadExt;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::PcmChunk;
use super::uri::StreamUri;
use crate::time::ChunkTimestamper;

/// Start a TCP listener that reads PCM from connecting clients.
pub fn start(
    uri: StreamUri,
    format: SampleFormat,
    tx: mpsc::Sender<PcmChunk>,
) -> Result<JoinHandle<()>> {
    let host = if uri.host.is_empty() {
        "0.0.0.0".to_string()
    } else {
        uri.host.clone()
    };
    let port = if uri.port == 0 { 4953 } else { uri.port };
    let addr = format!("{host}:{port}");
    let chunk_ms = 20;
    let chunk_bytes = (format.rate() as usize * format.frame_size() as usize * chunk_ms) / 1000;
    let chunk_frames = chunk_bytes / format.frame_size() as usize;

    Ok(tokio::spawn(async move {
        let listener = match TcpListener::bind(&addr).await {
            Ok(l) => {
                tracing::info!(addr, "TCP stream listening");
                l
            }
            Err(e) => {
                tracing::error!(addr, error = %e, "Failed to bind TCP stream");
                return;
            }
        };

        let mut ts = ChunkTimestamper::new(format.rate());
        loop {
            match listener.accept().await {
                Ok((mut stream, peer)) => {
                    tracing::info!(%peer, "TCP stream client connected");
                    let mut buf = vec![0u8; chunk_bytes];
                    loop {
                        match stream.read_exact(&mut buf).await {
                            Ok(_) => {
                                let chunk = PcmChunk {
                                    timestamp_usec: ts.next(chunk_frames as u32),
                                    data: buf.clone(),
                                };
                                if tx.send(chunk).await.is_err() {
                                    return;
                                }
                            }
                            Err(_) => {
                                tracing::info!(%peer, "TCP stream client disconnected");
                                break;
                            }
                        }
                    }
                    ts.reset();
                }
                Err(e) => {
                    tracing::error!(error = %e, "TCP accept failed");
                }
            }
        }
    }))
}
