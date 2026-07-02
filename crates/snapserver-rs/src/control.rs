//! Control server — JSON-RPC over TCP for Snapcast control clients.

use std::sync::Arc;

use anyhow::{Context, Result};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::sync::{broadcast, mpsc};

use crate::auth::AuthConfig;
use crate::jsonrpc::{self, RpcResult};

/// Configuration for the control server.
pub(crate) struct ControlConfig {
    /// TCP bind address.
    pub bind_address: String,
    /// TCP port.
    pub port: u16,
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
    let listener = TcpListener::bind((cfg.bind_address.as_str(), cfg.port)).await?;
    tracing::info!(
        bind_address = %cfg.bind_address,
        port = cfg.port,
        "Control server (TCP) listening"
    );

    loop {
        let (stream, peer) = listener.accept().await?;
        tracing::debug!(%peer, "Control client connected");

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

                        match jsonrpc::handle_request(&request, &auth_config, &cmd_tx).await {
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
                                    let _ = event_tx.send(crate::ControlEvent::JsonRpc {
                                        client_id: client_id.clone(),
                                        request,
                                        response_tx: None,
                                    }).await;
                                } else {
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

#[cfg(test)]
mod tests {
    //! Unit tests for the control-server wire framing.
    //!
    //! The bulk of this module is the TCP accept loop in [`run_tcp`] (bind /
    //! accept / `tokio::select!` over a `BufReader` line stream and a broadcast
    //! receiver). That is genuine socket I/O and the request-routing / auth-gate
    //! logic is written inline inside the accept loop rather than as callable
    //! functions, so it is only reachable through a live TCP connection and is
    //! covered by integration tests, not here. The one pure, cheaply-testable
    //! seam is [`send_json`], which serialises a JSON value and frames it with a
    //! trailing newline onto any `AsyncWrite`. We exercise it against an
    //! in-memory `Vec<u8>` buffer — no real sockets, fully deterministic.

    use super::*;

    /// A single value is serialised and terminated with exactly one newline.
    #[tokio::test]
    async fn send_json_appends_single_newline() {
        let mut buf: Vec<u8> = Vec::new();
        let value = serde_json::json!({"jsonrpc": "2.0", "id": 1, "result": {"ok": true}});

        send_json(&mut buf, &value).await.expect("send_json ok");

        let out = String::from_utf8(buf).expect("utf8");
        assert!(out.ends_with('\n'), "must be newline-terminated: {out:?}");
        assert_eq!(
            out.matches('\n').count(),
            1,
            "exactly one framing newline, got: {out:?}"
        );
    }

    /// The bytes written are exactly `serde_json::to_string(value)` + `\n`, so a
    /// client parsing a line back gets the same value.
    #[tokio::test]
    async fn send_json_matches_serde_serialization_plus_newline() {
        let mut buf: Vec<u8> = Vec::new();
        let value = serde_json::json!({"a": 1, "b": [true, null, "x"]});

        send_json(&mut buf, &value).await.expect("send_json ok");

        let out = String::from_utf8(buf).expect("utf8");
        let expected = format!("{}\n", serde_json::to_string(&value).unwrap());
        assert_eq!(out, expected);

        // And the framed line round-trips back to the original value.
        let line = out.strip_suffix('\n').unwrap();
        let reparsed: Value = serde_json::from_str(line).expect("reparse");
        assert_eq!(reparsed, value);
    }

    /// Two sequential sends append onto the same writer, producing two
    /// independently parseable newline-delimited frames (the on-wire protocol).
    #[tokio::test]
    async fn send_json_frames_multiple_messages() {
        let mut buf: Vec<u8> = Vec::new();
        let first = serde_json::json!({"id": 1});
        let second = serde_json::json!({"id": 2});

        send_json(&mut buf, &first).await.expect("first");
        send_json(&mut buf, &second).await.expect("second");

        let out = String::from_utf8(buf).expect("utf8");
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 2, "two frames expected: {out:?}");
        assert_eq!(serde_json::from_str::<Value>(lines[0]).unwrap(), first);
        assert_eq!(serde_json::from_str::<Value>(lines[1]).unwrap(), second);
    }

    /// A JSON `null` value is still framed (it is a valid whole message, e.g. a
    /// bare notification body); it must not be dropped or produce empty output.
    #[tokio::test]
    async fn send_json_handles_null_value() {
        let mut buf: Vec<u8> = Vec::new();

        send_json(&mut buf, &Value::Null)
            .await
            .expect("send_json ok");

        let out = String::from_utf8(buf).expect("utf8");
        assert_eq!(out, "null\n");
    }

    /// The parse-error envelope that the accept loop emits on malformed input
    /// serialises to a well-formed, newline-framed JSON-RPC error line.
    #[tokio::test]
    async fn send_json_frames_parse_error_envelope() {
        let mut buf: Vec<u8> = Vec::new();
        let err = serde_json::json!({
            "jsonrpc": "2.0", "id": null,
            "error": {"code": -32700, "message": "Parse error"}
        });

        send_json(&mut buf, &err).await.expect("send_json ok");

        let out = String::from_utf8(buf).expect("utf8");
        let line = out.strip_suffix('\n').expect("trailing newline");
        let parsed: Value = serde_json::from_str(line).expect("valid json line");
        assert_eq!(parsed["jsonrpc"], "2.0");
        assert_eq!(parsed["error"]["code"], -32700);
        assert_eq!(parsed["error"]["message"], "Parse error");
    }

    /// Non-ASCII content survives serialisation and round-trips unchanged, so
    /// the newline framing is byte-safe for the whole message.
    #[tokio::test]
    async fn send_json_preserves_unicode_and_stays_parseable() {
        let mut buf: Vec<u8> = Vec::new();
        let value = serde_json::json!({"name": "Wohnzimmer — Über", "emoji": "🎵"});

        send_json(&mut buf, &value).await.expect("send_json ok");

        let out = String::from_utf8(buf).expect("utf8");
        // Exactly one framing newline even with embedded multi-byte chars.
        assert_eq!(out.matches('\n').count(), 1);
        let line = out.strip_suffix('\n').unwrap();
        let reparsed: Value = serde_json::from_str(line).expect("reparse");
        assert_eq!(reparsed, value);
    }
}
