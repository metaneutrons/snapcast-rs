//! Control server — JSON-RPC over TCP for Snapcast control clients.

use std::sync::Arc;

use anyhow::{Context, Result};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::sync::{Mutex, broadcast, mpsc};

use crate::ServerEvent;
use crate::auth::AuthConfig;
use crate::jsonrpc::{self, ClientSettingsUpdate, RpcResult, StreamControlMsg};
use crate::state::ServerState;

/// Configuration for the control server.
pub struct ControlConfig {
    /// TCP port.
    pub port: u16,
    /// Shared server state.
    pub state: Arc<Mutex<ServerState>>,
    /// Event sender for extension point.
    pub event_tx: mpsc::Sender<ServerEvent>,
    /// Notification broadcast sender.
    pub notify_tx: broadcast::Sender<Value>,
    /// Auth configuration.
    pub auth_config: Arc<AuthConfig>,
    /// Stream control sender.
    pub stream_control_tx: mpsc::Sender<StreamControlMsg>,
    /// Client settings push sender.
    pub settings_tx: mpsc::Sender<ClientSettingsUpdate>,
    /// Server buffer size in ms.
    pub buffer_ms: i32,
}

/// Runs the JSON-RPC control server on a TCP port.
pub async fn run_tcp(cfg: ControlConfig) -> Result<()> {
    let listener = TcpListener::bind(format!("0.0.0.0:{}", cfg.port)).await?;
    tracing::info!(port = cfg.port, "Control server (TCP) listening");

    loop {
        let (stream, peer) = listener.accept().await?;
        tracing::debug!(%peer, "Control client connected");

        let state = Arc::clone(&cfg.state);
        let event_tx = cfg.event_tx.clone();
        let notify_tx = cfg.notify_tx.clone();
        let mut notify_rx = cfg.notify_tx.subscribe();
        let auth_config = Arc::clone(&cfg.auth_config);
        let stream_control_tx = cfg.stream_control_tx.clone();
        let settings_tx = cfg.settings_tx.clone();
        let buffer_ms = cfg.buffer_ms;

        tokio::spawn(async move {
            let (reader, mut writer) = stream.into_split();
            let mut lines = BufReader::new(reader).lines();
            let client_id = peer.to_string();
            let mut authenticated = !auth_config.enabled;

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

                        // Auth gate: allow Server.GetToken and Server.Authenticate without auth
                        let method = request["method"].as_str().unwrap_or("");
                        if !authenticated
                            && method != "Server.GetToken"
                            && method != "Server.Authenticate"
                        {
                            let err = serde_json::json!({
                                "jsonrpc": "2.0", "id": request["id"],
                                "error": {"code": -32000, "message": "Unauthorized — call Server.Authenticate first"}
                            });
                            let _ = send_json(&mut writer, &err).await;
                            continue;
                        }

                        match jsonrpc::handle_request(&request, &state, &auth_config, &stream_control_tx, &settings_tx, buffer_ms).await {
                            RpcResult::Response { response, notification } => {
                                // Mark as authenticated if Server.Authenticate succeeded
                                if method == "Server.Authenticate" && response["result"]["ok"] == true {
                                    authenticated = true;
                                }
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
