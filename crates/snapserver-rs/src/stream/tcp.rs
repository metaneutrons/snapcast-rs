//! TCP stream reader — accepts a TCP connection and reads PCM from it.

use anyhow::Result;
use snapcast_proto::SampleFormat;
use tokio::io::AsyncReadExt;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::uri::StreamUri;
use snapcast_server::time::ChunkTimestamper;
use snapcast_server::{AudioData, AudioFrame};

/// Start a TCP listener that reads PCM from connecting clients.
pub fn start(
    uri: StreamUri,
    format: SampleFormat,
    chunk_frames: usize,
    tx: mpsc::Sender<AudioFrame>,
) -> Result<JoinHandle<()>> {
    let host = if uri.host.is_empty() {
        snapcast_proto::DEFAULT_BIND_ADDRESS.to_string()
    } else {
        uri.host.clone()
    };
    let port = if uri.port == 0 { 4953 } else { uri.port };
    let chunk_bytes = chunk_frames * format.frame_size() as usize;

    Ok(tokio::spawn(async move {
        let listener = match TcpListener::bind((host.as_str(), port)).await {
            Ok(l) => {
                tracing::info!(bind_address = %host, port, "TCP stream listening");
                l
            }
            Err(e) => {
                tracing::error!(bind_address = %host, port, error = %e, "Failed to bind TCP stream");
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
                                let frame = AudioFrame {
                                    timestamp_usec: ts.next(chunk_frames as u32),
                                    data: AudioData::Pcm(buf.clone()),
                                };
                                if tx.send(frame).await.is_err() {
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
