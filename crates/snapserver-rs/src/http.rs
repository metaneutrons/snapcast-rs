//! HTTP/WebSocket control server + Snapweb static file serving.

use std::sync::Arc;

use anyhow::Result;
use axum::Router;
use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::IntoResponse;
use axum::routing::get;
use serde_json::Value;
use tokio::sync::{Mutex, broadcast, mpsc};

use crate::auth::AuthConfig;
use crate::jsonrpc::{self, RpcResult};
use snapcast_server::state::ServerState;

/// Shared state for axum handlers.
#[derive(Clone)]
struct AppState {
    state: Arc<Mutex<ServerState>>,
    event_tx: mpsc::Sender<crate::ControlEvent>,
    notify_tx: broadcast::Sender<Value>,
    auth_config: Arc<AuthConfig>,
    cmd_tx: tokio::sync::mpsc::Sender<snapcast_server::ServerCommand>,
}

/// Configuration for the HTTP server.
pub struct HttpConfig {
    /// HTTP port.
    pub port: u16,
    /// Snapweb document root (None = disabled).
    pub doc_root: Option<String>,
    /// Shared server state.
    pub state: Arc<Mutex<ServerState>>,
    /// Event sender for extension point.
    pub event_tx: mpsc::Sender<crate::ControlEvent>,
    /// Notification broadcast sender.
    pub notify_tx: broadcast::Sender<Value>,
    /// Auth configuration.
    pub auth_config: Arc<AuthConfig>,
    /// Stream control sender.
    /// Client settings push sender.
    /// Server buffer size in ms.
    /// Server command sender.
    pub cmd_tx: tokio::sync::mpsc::Sender<snapcast_server::ServerCommand>,
}

/// Start the HTTP server with JSON-RPC + WebSocket + optional Snapweb.
pub async fn run_http(cfg: HttpConfig) -> Result<()> {
    let app_state = AppState {
        state: cfg.state,
        event_tx: cfg.event_tx,
        notify_tx: cfg.notify_tx,
        auth_config: cfg.auth_config,
        cmd_tx: cfg.cmd_tx.clone(),
    };

    let mut app = Router::new()
        .route("/jsonrpc", get(ws_handler).post(http_jsonrpc_handler))
        .with_state(app_state);

    // Serve Snapweb static files if doc_root is set
    if let Some(ref root) = cfg.doc_root {
        let serve = tower_http::services::ServeDir::new(root);
        app = app.fallback_service(serve);
        tracing::info!(doc_root = root, "Serving Snapweb");
    }

    let addr = format!("0.0.0.0:{}", cfg.port);
    tracing::debug!(addr, "HTTP: binding");
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!(port = cfg.port, "HTTP/WebSocket server listening");
    axum::serve(listener, app).await?;
    Ok(())
}

/// HTTP POST /jsonrpc handler.
async fn http_jsonrpc_handler(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
    body: String,
) -> impl IntoResponse {
    // Auth check
    let auth_header = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());
    if let Err(e) = crate::auth::validate_bearer(&app.auth_config, auth_header) {
        return axum::Json(serde_json::json!({
            "jsonrpc": "2.0", "id": null,
            "error": {"code": -32000, "message": format!("Unauthorized: {e}")}
        }));
    }

    let Ok(request) = serde_json::from_str::<Value>(&body) else {
        return axum::Json(serde_json::json!({
            "jsonrpc": "2.0", "id": null,
            "error": {"code": -32700, "message": "Parse error"}
        }));
    };

    match jsonrpc::handle_request(&request, &app.state, &app.auth_config, &app.cmd_tx).await {
        RpcResult::Response {
            response,
            notification,
        } => {
            if let Some(n) = notification {
                let _ = app.notify_tx.send(n);
            }
            axum::Json(response)
        }
        RpcResult::Unknown => {
            let _ = app
                .event_tx
                .send(crate::ControlEvent::JsonRpc {
                    response_tx: None,
                    client_id: "http".into(),
                    request,
                })
                .await;
            axum::Json(serde_json::json!({
                "jsonrpc": "2.0", "id": null,
                "error": {"code": -32601, "message": "Method not found"}
            }))
        }
    }
}

/// WebSocket upgrade handler at GET /jsonrpc.
async fn ws_handler(ws: WebSocketUpgrade, State(app): State<AppState>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_ws(socket, app))
}

async fn handle_ws(mut socket: WebSocket, app: AppState) {
    let mut notify_rx = app.notify_tx.subscribe();

    loop {
        tokio::select! {
            msg = socket.recv() => {
                let Some(Ok(msg)) = msg else { break };
                let Message::Text(text) = msg else { continue };

                let Ok(request) = serde_json::from_str::<Value>(&text) else {
                    let err = serde_json::json!({
                        "jsonrpc": "2.0", "id": null,
                        "error": {"code": -32700, "message": "Parse error"}
                    });
                    if socket.send(Message::Text(err.to_string().into())).await.is_err() { break }
                    continue;
                };

                match jsonrpc::handle_request(
                    &request, &app.state, &app.auth_config,
                    &app.cmd_tx,
                ).await {
                    RpcResult::Response { response, notification } => {
                        if socket.send(Message::Text(response.to_string().into())).await.is_err() { break }
                        if let Some(n) = notification {
                            let _ = app.notify_tx.send(n);
                        }
                    }
                    RpcResult::Unknown => {
                        let _ = app.event_tx.send(crate::ControlEvent::JsonRpc { response_tx: None,
                            client_id: "websocket".into(),
                            request,
                        }).await;
                    }
                }
            }
            notification = notify_rx.recv() => {
                if let Ok(n) = notification
                    && socket.send(Message::Text(n.to_string().into())).await.is_err()
                {
                    break;
                }
            }
        }
    }
}
