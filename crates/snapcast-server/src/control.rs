//! Control server — JSON-RPC over TCP for Snapcast control clients.

use std::sync::Arc;

use anyhow::{Context, Result};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::sync::{Mutex, broadcast, mpsc};

use crate::ServerEvent;
use crate::auth::AuthConfig;
use crate::jsonrpc::{self, RpcResult, StreamControlMsg};
use crate::state::ServerState;

/// Runs the JSON-RPC control server on a TCP port.
pub async fn run_tcp(
    port: u16,
    state: Arc<Mutex<ServerState>>,
    event_tx: mpsc::Sender<ServerEvent>,
    notify_tx: broadcast::Sender<Value>,
    auth_config: Arc<AuthConfig>,
    stream_control_tx: mpsc::Sender<StreamControlMsg>,
) -> Result<()> {
    let listener = TcpListener::bind(format!("0.0.0.0:{port}")).await?;
    tracing::info!(port, "Control server (TCP) listening");

    loop {
        let (stream, peer) = listener.accept().await?;
        tracing::debug!(%peer, "Control client connected");

        let state = Arc::clone(&state);
        let event_tx = event_tx.clone();
        let notify_tx = notify_tx.clone();
        let mut notify_rx = notify_tx.subscribe();
        let auth_config = Arc::clone(&auth_config);
        let stream_control_tx = stream_control_tx.clone();

        tokio::spawn(async move {
            let (reader, mut writer) = stream.into_split();
            let mut lines = BufReader::new(reader).lines();
            let client_id = peer.to_string();

            loop {
                tokio::select! {
                    line = lines.next_line() => {
                        let Ok(Some(line)) = line else { break };
                        if line.trim().is_empty() { continue; }

                        let Ok(request) = serde_json::from_str::<Value>(&line) else {
                            let err = serde_json::json!({
                                "jsonrpc": "2.0", "id": null,
                                "error": {"code": -32700, "message": "Parse error"}
                            });
                            let _ = send_json(&mut writer, &err).await;
                            continue;
                        };

                        match jsonrpc::handle_request(&request, &state, &auth_config, &stream_control_tx).await {
                            RpcResult::Response { response, notification } => {
                                let _ = send_json(&mut writer, &response).await;
                                if let Some(n) = notification {
                                    let _ = notify_tx.send(n);
                                }
                            }
                            RpcResult::Unknown => {
                                // Extension point: forward to embedding app
                                let _ = event_tx.send(ServerEvent::JsonRpc {
                                    client_id: client_id.clone(),
                                    request,
                                }).await;
                            }
                        }
                    }
                    notification = notify_rx.recv() => {
                        if let Ok(n) = notification
                            && send_json(&mut writer, &n).await.is_err()
                        {
                            break;
                        }
                    }
                }
            }
            tracing::debug!(%peer, "Control client disconnected");
        });
    }
}

async fn send_json<W: AsyncWriteExt + Unpin>(writer: &mut W, value: &Value) -> Result<()> {
    let mut msg = serde_json::to_string(value)?;
    msg.push('\n');
    writer.write_all(msg.as_bytes()).await.context("write json")
}
