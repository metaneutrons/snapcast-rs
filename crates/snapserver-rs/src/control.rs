//! Control server — JSON-RPC over TCP for Snapcast control clients.

use std::sync::Arc;

use anyhow::{Context, Result};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::sync::{Mutex, broadcast, mpsc};

use crate::auth::AuthConfig;
use crate::jsonrpc::{self, RpcResult};
use snapcast_server::state::ServerState;

/// Configuration for the control server.
pub(crate) struct ControlConfig {
    /// TCP port.
    pub port: u16,
    /// Shared server state.
    pub state: Arc<Mutex<ServerState>>,
    /// Event sender for extension point.
    pub event_tx: mpsc::Sender<crate::ControlEvent>,
    /// Notification broadcast sender.
    pub notify_tx: broadcast::Sender<Value>,
    /// Auth configuration.
    pub auth_config: Arc<AuthConfig>,
    /// Server command sender.
    pub cmd_tx: tokio::sync::mpsc::Sender<snapcast_server::ServerCommand>,
    /// Registered custom JSON-RPC methods.
    pub registered_methods: Arc<std::collections::HashSet<String>>,
    /// Registered custom JSON-RPC notifications.
    pub registered_notifications: Arc<std::collections::HashSet<String>>,
}

/// Runs the JSON-RPC control server on a TCP port.
pub(crate) async fn run_tcp(cfg: ControlConfig) -> Result<()> {
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
        let cmd_tx = cfg.cmd_tx.clone();
        let registered_methods = Arc::clone(&cfg.registered_methods);
        let registered_notifications = Arc::clone(&cfg.registered_notifications);

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

                        match jsonrpc::handle_request(&request, &state, &auth_config, &cmd_tx).await {
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
                                let method_str = method.to_string();
                                if registered_methods.contains(&method_str) {
                                    // Registered method: forward and wait for response
                                    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
                                    let _ = event_tx.send(crate::ControlEvent::JsonRpc {
                                        client_id: client_id.clone(),
                                        request,
                                        response_tx: Some(resp_tx),
                                    }).await;
                                    match tokio::time::timeout(
                                        std::time::Duration::from_secs(5),
                                        resp_rx,
                                    ).await {
                                        Ok(Ok(response)) => {
                                            let _ = send_json(&mut writer, &response).await;
                                        }
                                        _ => {
                                            let err = serde_json::json!({
                                                "jsonrpc": "2.0", "id": null,
                                                "error": {"code": -32603, "message": "Handler timeout"}
                                            });
                                            let _ = send_json(&mut writer, &err).await;
                                        }
                                    }
                                } else if registered_notifications.contains(&method_str) {
                                    // Registered notification: forward, no response
                                    let _ = event_tx.send(crate::ControlEvent::JsonRpc {
                                        client_id: client_id.clone(),
                                        request,
                                        response_tx: None,
                                    }).await;
                                } else {
                                    // Unknown method
                                    let err = serde_json::json!({
                                        "jsonrpc": "2.0", "id": request["id"],
                                        "error": {"code": -32601, "message": "Method not found"}
                                    });
                                    let _ = send_json(&mut writer, &err).await;
                                }
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
