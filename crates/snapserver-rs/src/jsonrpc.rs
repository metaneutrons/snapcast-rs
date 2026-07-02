//! JSON-RPC control API — method handlers for Snapcast control protocol.

use serde_json::{Value, json};

use crate::auth::{self, AuthConfig};

/// JSON-RPC error codes.
const INVALID_PARAMS: i64 = -32602;

/// Bind a required string parameter, or return an `INVALID_PARAMS` error naming
/// the missing field. Replaces the `let Some(x) = params["k"].as_str() else {
/// return err(...) }` boilerplate repeated by every handler.
macro_rules! require_str {
    ($params:expr, $key:literal, $id:expr) => {
        match $params[$key].as_str() {
            Some(value) => value,
            None => return err($id, INVALID_PARAMS, concat!("missing '", $key, "'")),
        }
    };
}

/// Result of handling a JSON-RPC request.
pub(crate) enum RpcResult {
    /// Handled: response JSON + optional notification to broadcast.
    Response {
        /// JSON-RPC response.
        response: Value,
        /// Optional notification to broadcast to all control clients.
        notification: Option<Value>,
    },
    /// Method not recognized — forward to extension handler.
    Unknown,
}

/// Fetch server status via GetStatus command, serialized to JSON.
async fn get_status(
    cmd_tx: &tokio::sync::mpsc::Sender<snapcast_server::ServerCommand>,
) -> Option<Value> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    cmd_tx
        .send(snapcast_server::ServerCommand::GetStatus { response_tx: tx })
        .await
        .ok()?;
    let status = rx.await.ok()?;
    serde_json::to_value(status).ok()
}

/// Handle a JSON-RPC request. All state access goes through ServerCommand.
pub(crate) async fn handle_request(
    request: &Value,
    auth_config: &AuthConfig,
    cmd_tx: &tokio::sync::mpsc::Sender<snapcast_server::ServerCommand>,
) -> RpcResult {
    let id = &request["id"];
    let method = request["method"].as_str().unwrap_or("");
    let params = &request["params"];

    match method {
        // --- Server ---
        "Server.GetRPCVersion" => ok(id, json!({"major": 2, "minor": 0, "patch": 0})),
        "Server.GetStatus" => match get_status(cmd_tx).await {
            Some(status) => ok(id, status),
            None => err(id, INVALID_PARAMS, "status unavailable"),
        },
        "Server.DeleteClient" => {
            let client_id = require_str!(params, "id", id);
            let _ = cmd_tx
                .send(snapcast_server::ServerCommand::DeleteClient {
                    client_id: client_id.to_string(),
                })
                .await;
            ok(id, json!({"id": client_id}))
        }

        // --- Client ---
        "Client.GetStatus" => {
            let client_id = require_str!(params, "id", id);
            let Some(status) = get_status(cmd_tx).await else {
                return err(id, INVALID_PARAMS, "status unavailable");
            };
            let client = status["server"]["groups"]
                .as_array()
                .into_iter()
                .flatten()
                .flat_map(|g| g["clients"].as_array().into_iter().flatten())
                .find(|c| c["id"].as_str() == Some(client_id));
            match client {
                Some(c) => ok(id, json!({"client": c})),
                None => err(id, INVALID_PARAMS, "client not found"),
            }
        }
        "Client.SetVolume" => {
            let client_id = require_str!(params, "id", id);
            let volume = params["volume"]["percent"].as_u64().unwrap_or(100).min(100) as u16;
            let muted = params["volume"]["muted"].as_bool().unwrap_or(false);
            let _ = cmd_tx
                .send(snapcast_server::ServerCommand::SetClientVolume {
                    client_id: client_id.to_string(),
                    volume,
                    muted,
                })
                .await;
            let vol = json!({"percent": volume, "muted": muted});
            ok_with_notification(
                id,
                json!({"volume": vol}),
                crate::notify::client_on_volume_changed(client_id, volume, muted),
            )
        }
        "Client.SetLatency" => {
            let client_id = require_str!(params, "id", id);
            let latency = params["latency"].as_i64().unwrap_or(0) as i32;
            let _ = cmd_tx
                .send(snapcast_server::ServerCommand::SetClientLatency {
                    client_id: client_id.to_string(),
                    latency,
                })
                .await;
            ok_with_notification(
                id,
                json!({"latency": latency}),
                crate::notify::client_on_latency_changed(client_id, latency),
            )
        }
        "Client.SetName" => {
            let client_id = require_str!(params, "id", id);
            let name = params["name"].as_str().unwrap_or("").to_string();
            let _ = cmd_tx
                .send(snapcast_server::ServerCommand::SetClientName {
                    client_id: client_id.to_string(),
                    name: name.clone(),
                })
                .await;
            ok_with_notification(
                id,
                json!({"name": &name}),
                crate::notify::client_on_name_changed(client_id, &name),
            )
        }

        // --- Group ---
        "Group.GetStatus" => {
            let group_id = require_str!(params, "id", id);
            let Some(status) = get_status(cmd_tx).await else {
                return err(id, INVALID_PARAMS, "status unavailable");
            };
            let group = status["server"]["groups"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|g| g["id"].as_str() == Some(group_id));
            match group {
                Some(g) => ok(id, json!({"group": g})),
                None => err(id, INVALID_PARAMS, "group not found"),
            }
        }
        "Group.SetMute" => {
            let group_id = require_str!(params, "id", id);
            let muted = params["mute"].as_bool().unwrap_or(false);
            let _ = cmd_tx
                .send(snapcast_server::ServerCommand::SetGroupMute {
                    group_id: group_id.to_string(),
                    muted,
                })
                .await;
            ok_with_notification(
                id,
                json!({"mute": muted}),
                crate::notify::group_on_mute(group_id, muted),
            )
        }
        "Group.SetStream" => {
            let group_id = require_str!(params, "id", id);
            let stream_id = require_str!(params, "stream_id", id);
            let _ = cmd_tx
                .send(snapcast_server::ServerCommand::SetGroupStream {
                    group_id: group_id.to_string(),
                    stream_id: stream_id.to_string(),
                })
                .await;
            ok_with_notification(
                id,
                json!({"stream_id": stream_id}),
                crate::notify::group_on_stream_changed(group_id, stream_id),
            )
        }
        "Group.SetClients" => {
            let group_id = require_str!(params, "id", id);
            let Some(clients) = params["clients"].as_array() else {
                return err(id, INVALID_PARAMS, "missing 'clients'");
            };
            let client_ids: Vec<String> = clients
                .iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect();
            let _ = cmd_tx
                .send(snapcast_server::ServerCommand::SetGroupClients {
                    group_id: group_id.to_string(),
                    clients: client_ids,
                })
                .await;
            let status = get_status(cmd_tx).await.unwrap_or_default();
            ok_with_notify(id, status.clone(), "Server.OnUpdate", status)
        }
        "Group.SetName" => {
            let group_id = require_str!(params, "id", id);
            let name = params["name"].as_str().unwrap_or("").to_string();
            let _ = cmd_tx
                .send(snapcast_server::ServerCommand::SetGroupName {
                    group_id: group_id.to_string(),
                    name: name.clone(),
                })
                .await;
            ok_with_notification(
                id,
                json!({"name": &name}),
                crate::notify::group_on_name_changed(group_id, &name),
            )
        }

        // --- Stream ---
        "Stream.SetProperty" => {
            let stream_id = require_str!(params, "id", id);
            let metadata = params["properties"]
                .as_object()
                .map(|o| o.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
                .unwrap_or_default();
            let _ = cmd_tx
                .send(snapcast_server::ServerCommand::SetStreamMeta {
                    stream_id: stream_id.to_string(),
                    metadata,
                })
                .await;
            let props = params["properties"].clone();
            ok_with_notify(
                id,
                json!({"id": stream_id, "properties": &props}),
                "Stream.OnUpdate",
                json!({"id": stream_id, "properties": props}),
            )
        }
        "Stream.Control" => {
            let stream_id = require_str!(params, "id", id);
            let command = require_str!(params, "command", id);
            let _ = cmd_tx
                .send(snapcast_server::ServerCommand::StreamControl {
                    stream_id: stream_id.to_string(),
                    command: command.to_string(),
                    params: params["params"].clone(),
                })
                .await;
            ok(id, json!({"id": stream_id}))
        }
        "Stream.AddStream" => {
            let stream_uri = require_str!(params, "streamUri", id);
            let (tx, rx) = tokio::sync::oneshot::channel();
            let _ = cmd_tx
                .send(snapcast_server::ServerCommand::AddStream {
                    uri: stream_uri.to_string(),
                    response_tx: tx,
                })
                .await;
            match rx.await {
                Ok(Ok(stream_id)) => {
                    let status = get_status(cmd_tx).await.unwrap_or_default();
                    ok_with_notify(id, json!({"id": stream_id}), "Server.OnUpdate", status)
                }
                Ok(Err(e)) => err(id, INVALID_PARAMS, &e),
                Err(_) => err(id, INVALID_PARAMS, "command failed"),
            }
        }
        "Stream.RemoveStream" => {
            let stream_id = require_str!(params, "id", id);
            let _ = cmd_tx
                .send(snapcast_server::ServerCommand::RemoveStream {
                    stream_id: stream_id.to_string(),
                })
                .await;
            let status = get_status(cmd_tx).await.unwrap_or_default();
            ok_with_notify(id, json!({"id": stream_id}), "Server.OnUpdate", status)
        }

        // --- Auth ---
        "Server.GetToken" => {
            // SECURITY (known gap): this mints a valid token for ANY username
            // with no credential check. The auth gate (TCP/WS/HTTP) is therefore
            // necessary but not sufficient — real security needs GetToken to
            // verify credentials against a user store before issuing a token.
            // Tracked separately; do not treat enabled auth as a security
            // boundary until this is implemented.
            let username = params["username"].as_str().unwrap_or("anonymous");
            match auth::generate_token(auth_config, username) {
                Ok(token) => ok(id, json!({"token": token})),
                Err(e) => err(id, INVALID_PARAMS, &format!("token generation failed: {e}")),
            }
        }
        "Server.Authenticate" => {
            let token = require_str!(params, "token", id);
            match auth::validate_token(auth_config, token) {
                Ok(subject) => ok(id, json!({"ok": true, "subject": subject})),
                Err(_) => err(id, INVALID_PARAMS, "invalid token"),
            }
        }

        _ => RpcResult::Unknown,
    }
}

fn ok(id: &Value, result: Value) -> RpcResult {
    RpcResult::Response {
        response: json!({"jsonrpc": "2.0", "id": id, "result": result}),
        notification: None,
    }
}

fn ok_with_notify(id: &Value, result: Value, method: &str, params: Value) -> RpcResult {
    RpcResult::Response {
        response: json!({"jsonrpc": "2.0", "id": id, "result": result}),
        notification: Some(json!({"jsonrpc": "2.0", "method": method, "params": params})),
    }
}

/// Like [`ok_with_notify`] but takes a pre-built notification object (from the
/// [`crate::notify`] builders) so the notification shape has a single source.
fn ok_with_notification(id: &Value, result: Value, notification: Value) -> RpcResult {
    RpcResult::Response {
        response: json!({"jsonrpc": "2.0", "id": id, "result": result}),
        notification: Some(notification),
    }
}

fn err(id: &Value, code: i64, msg: &str) -> RpcResult {
    RpcResult::Response {
        response: json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": msg}}),
        notification: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use snapcast_server::{ServerCommand, status};

    /// Spawn a mock command handler that processes GetStatus and SetClientVolume.
    fn mock_server() -> (AuthConfig, tokio::sync::mpsc::Sender<ServerCommand>) {
        let (cmd_tx, mut cmd_rx) = tokio::sync::mpsc::channel::<ServerCommand>(16);
        tokio::spawn(async move {
            let mut volume: u16 = 100;
            let mut muted = false;
            while let Some(cmd) = cmd_rx.recv().await {
                match cmd {
                    ServerCommand::GetStatus { response_tx } => {
                        let _ = response_tx.send(status::ServerStatus {
                            server: status::Server {
                                groups: vec![status::Group {
                                    id: "g1".into(),
                                    stream_id: "default".into(),
                                    clients: vec![status::Client {
                                        id: "c1".into(),
                                        connected: true,
                                        config: status::ClientConfig {
                                            volume: status::Volume {
                                                percent: volume,
                                                muted,
                                            },
                                            ..Default::default()
                                        },
                                        host: status::Host {
                                            name: "host1".into(),
                                            mac: "mac1".into(),
                                            ..Default::default()
                                        },
                                        ..Default::default()
                                    }],
                                    ..Default::default()
                                }],
                                streams: vec![status::Stream {
                                    id: "default".into(),
                                    status: status::StreamStatus::Playing,
                                    ..Default::default()
                                }],
                                ..Default::default()
                            },
                        });
                    }
                    ServerCommand::SetClientVolume {
                        volume: v,
                        muted: m,
                        ..
                    } => {
                        volume = v;
                        muted = m;
                    }
                    _ => {}
                }
            }
        });
        (AuthConfig::default(), cmd_tx)
    }

    #[tokio::test]
    async fn server_get_status() {
        let (auth_config, cmd_tx) = mock_server();
        let req = json!({"jsonrpc": "2.0", "id": 1, "method": "Server.GetStatus", "params": {}});
        let RpcResult::Response { response, .. } =
            handle_request(&req, &auth_config, &cmd_tx).await
        else {
            panic!("expected response");
        };
        assert!(response["result"]["server"]["groups"].is_array());
    }

    #[tokio::test]
    async fn client_set_volume() {
        let (auth_config, cmd_tx) = mock_server();
        let req = json!({
            "jsonrpc": "2.0", "id": 2,
            "method": "Client.SetVolume",
            "params": {"id": "c1", "volume": {"percent": 50, "muted": true}}
        });
        let RpcResult::Response {
            response,
            notification,
        } = handle_request(&req, &auth_config, &cmd_tx).await
        else {
            panic!("expected response");
        };
        assert_eq!(response["result"]["volume"]["percent"], 50);
        assert!(notification.is_some());
        assert_eq!(notification.unwrap()["method"], "Client.OnVolumeChanged");

        // Verify state updated via GetStatus
        tokio::task::yield_now().await;
        let status = get_status(&cmd_tx).await.unwrap();
        assert_eq!(
            status["server"]["groups"][0]["clients"][0]["config"]["volume"]["percent"],
            50
        );
    }

    #[tokio::test]
    async fn unknown_method_returns_unknown() {
        let (auth_config, cmd_tx) = mock_server();
        let req = json!({"jsonrpc": "2.0", "id": 3, "method": "Client.SetEq", "params": {}});
        assert!(matches!(
            handle_request(&req, &auth_config, &cmd_tx).await,
            RpcResult::Unknown
        ));
    }

    #[tokio::test]
    async fn group_set_stream() {
        let (auth_config, cmd_tx) = mock_server();
        let req = json!({
            "jsonrpc": "2.0", "id": 4,
            "method": "Group.SetStream",
            "params": {"id": "g1", "stream_id": "music"}
        });
        let RpcResult::Response { notification, .. } =
            handle_request(&req, &auth_config, &cmd_tx).await
        else {
            panic!("expected response");
        };
        assert_eq!(notification.unwrap()["params"]["stream_id"], "music");
    }

    // === Wave-4 additions ===================================================
    //
    // These cover the remaining request->response logic of every dispatchable
    // method: happy path plus the error paths (missing/invalid params, unknown
    // method, malformed method field, wrong id echoing). Socket transport and
    // the real ServerCommand executor are out of scope — the mock command sink
    // stands in for state. Fire-and-forget commands need no mock reply, so most
    // handlers exercise here via the shared `mock_server()`; handlers that await
    // a reply (`Stream.AddStream`) use the dedicated helper below.

    /// Unwrap a `RpcResult::Response`, panicking on `Unknown`.
    fn resp(result: RpcResult) -> (Value, Option<Value>) {
        match result {
            RpcResult::Response {
                response,
                notification,
            } => (response, notification),
            RpcResult::Unknown => panic!("expected Response, got Unknown"),
        }
    }

    /// A command sink that answers `AddStream` with the supplied result and
    /// still serves `GetStatus` (needed for the post-add `Server.OnUpdate`).
    fn mock_server_addstream(
        result: Result<String, String>,
    ) -> (AuthConfig, tokio::sync::mpsc::Sender<ServerCommand>) {
        let (cmd_tx, mut cmd_rx) = tokio::sync::mpsc::channel::<ServerCommand>(16);
        tokio::spawn(async move {
            let mut result = Some(result);
            while let Some(cmd) = cmd_rx.recv().await {
                match cmd {
                    ServerCommand::AddStream { response_tx, .. } => {
                        if let Some(r) = result.take() {
                            let _ = response_tx.send(r);
                        }
                    }
                    ServerCommand::GetStatus { response_tx } => {
                        let _ = response_tx.send(status::ServerStatus::default());
                    }
                    _ => {}
                }
            }
        });
        (AuthConfig::default(), cmd_tx)
    }

    /// An auth-enabled config with a real secret, plus a matching valid token.
    fn auth_enabled() -> (AuthConfig, tokio::sync::mpsc::Sender<ServerCommand>) {
        let (_disabled, cmd_tx) = mock_server();
        let config = AuthConfig {
            enabled: true,
            secret: "wave4-test-secret-at-least-32-bytes!".into(),
        };
        (config, cmd_tx)
    }

    // --- Envelope helpers ------------------------------------------------

    #[test]
    fn ok_envelope_shape() {
        let (response, notification) = resp(ok(&json!(7), json!({"a": 1})));
        assert_eq!(response["jsonrpc"], "2.0");
        assert_eq!(response["id"], 7);
        assert_eq!(response["result"]["a"], 1);
        assert!(response.get("error").is_none());
        assert!(notification.is_none());
    }

    #[test]
    fn err_envelope_shape() {
        let (response, notification) = resp(err(&json!("abc"), INVALID_PARAMS, "boom"));
        assert_eq!(response["jsonrpc"], "2.0");
        assert_eq!(response["id"], "abc");
        assert_eq!(response["error"]["code"], INVALID_PARAMS);
        assert_eq!(response["error"]["message"], "boom");
        assert!(response.get("result").is_none());
        assert!(notification.is_none());
    }

    #[test]
    fn ok_with_notify_pairs_response_and_notification() {
        let (response, notification) = resp(ok_with_notify(
            &json!(1),
            json!({"r": true}),
            "Server.OnUpdate",
            json!({"p": 2}),
        ));
        assert_eq!(response["result"]["r"], true);
        let n = notification.expect("notification present");
        assert_eq!(n["jsonrpc"], "2.0");
        assert_eq!(n["method"], "Server.OnUpdate");
        assert_eq!(n["params"]["p"], 2);
    }

    // --- Server.* --------------------------------------------------------

    #[tokio::test]
    async fn server_get_rpc_version() {
        let (auth_config, cmd_tx) = mock_server();
        let req = json!({"jsonrpc": "2.0", "id": 10, "method": "Server.GetRPCVersion"});
        let (response, notification) = resp(handle_request(&req, &auth_config, &cmd_tx).await);
        assert_eq!(
            response["result"],
            json!({"major": 2, "minor": 0, "patch": 0})
        );
        assert!(notification.is_none());
    }

    #[tokio::test]
    async fn server_delete_client_echoes_id() {
        let (auth_config, cmd_tx) = mock_server();
        let req = json!({
            "jsonrpc": "2.0", "id": 11,
            "method": "Server.DeleteClient", "params": {"id": "c1"}
        });
        let (response, _) = resp(handle_request(&req, &auth_config, &cmd_tx).await);
        assert_eq!(response["result"]["id"], "c1");
        assert_eq!(response["id"], 11);
    }

    #[tokio::test]
    async fn server_delete_client_missing_id_is_invalid_params() {
        let (auth_config, cmd_tx) = mock_server();
        let req =
            json!({"jsonrpc": "2.0", "id": 12, "method": "Server.DeleteClient", "params": {}});
        let (response, _) = resp(handle_request(&req, &auth_config, &cmd_tx).await);
        assert_eq!(response["error"]["code"], INVALID_PARAMS);
        assert_eq!(response["error"]["message"], "missing 'id'");
    }

    // --- Client.* --------------------------------------------------------

    #[tokio::test]
    async fn client_get_status_found() {
        let (auth_config, cmd_tx) = mock_server();
        let req = json!({
            "jsonrpc": "2.0", "id": 20,
            "method": "Client.GetStatus", "params": {"id": "c1"}
        });
        let (response, _) = resp(handle_request(&req, &auth_config, &cmd_tx).await);
        assert_eq!(response["result"]["client"]["id"], "c1");
    }

    #[tokio::test]
    async fn client_get_status_not_found() {
        let (auth_config, cmd_tx) = mock_server();
        let req = json!({
            "jsonrpc": "2.0", "id": 21,
            "method": "Client.GetStatus", "params": {"id": "nope"}
        });
        let (response, _) = resp(handle_request(&req, &auth_config, &cmd_tx).await);
        assert_eq!(response["error"]["code"], INVALID_PARAMS);
        assert_eq!(response["error"]["message"], "client not found");
    }

    #[tokio::test]
    async fn client_get_status_missing_id() {
        let (auth_config, cmd_tx) = mock_server();
        let req = json!({"jsonrpc": "2.0", "id": 22, "method": "Client.GetStatus", "params": {}});
        let (response, _) = resp(handle_request(&req, &auth_config, &cmd_tx).await);
        assert_eq!(response["error"]["message"], "missing 'id'");
    }

    #[tokio::test]
    async fn client_set_volume_clamps_above_100() {
        let (auth_config, cmd_tx) = mock_server();
        let req = json!({
            "jsonrpc": "2.0", "id": 23,
            "method": "Client.SetVolume",
            "params": {"id": "c1", "volume": {"percent": 250, "muted": false}}
        });
        let (response, notification) = resp(handle_request(&req, &auth_config, &cmd_tx).await);
        // percent > 100 is clamped to 100.
        assert_eq!(response["result"]["volume"]["percent"], 100);
        let n = notification.expect("notification present");
        assert_eq!(n["params"]["volume"]["percent"], 100);
    }

    #[tokio::test]
    async fn client_set_volume_defaults_when_fields_missing_or_wrong_type() {
        let (auth_config, cmd_tx) = mock_server();
        // No `volume` object at all: percent defaults to 100, muted to false.
        let req = json!({
            "jsonrpc": "2.0", "id": 24,
            "method": "Client.SetVolume", "params": {"id": "c1"}
        });
        let (response, _) = resp(handle_request(&req, &auth_config, &cmd_tx).await);
        assert_eq!(response["result"]["volume"]["percent"], 100);
        assert_eq!(response["result"]["volume"]["muted"], false);

        // `muted` present but wrong type (string) -> defaults to false.
        let req = json!({
            "jsonrpc": "2.0", "id": 25,
            "method": "Client.SetVolume",
            "params": {"id": "c1", "volume": {"percent": 30, "muted": "yes"}}
        });
        let (response, _) = resp(handle_request(&req, &auth_config, &cmd_tx).await);
        assert_eq!(response["result"]["volume"]["percent"], 30);
        assert_eq!(response["result"]["volume"]["muted"], false);
    }

    #[tokio::test]
    async fn client_set_volume_missing_id() {
        let (auth_config, cmd_tx) = mock_server();
        let req = json!({
            "jsonrpc": "2.0", "id": 26,
            "method": "Client.SetVolume", "params": {"volume": {"percent": 10}}
        });
        let (response, notification) = resp(handle_request(&req, &auth_config, &cmd_tx).await);
        assert_eq!(response["error"]["message"], "missing 'id'");
        assert!(notification.is_none());
    }

    #[tokio::test]
    async fn client_set_latency_happy_path() {
        let (auth_config, cmd_tx) = mock_server();
        let req = json!({
            "jsonrpc": "2.0", "id": 27,
            "method": "Client.SetLatency", "params": {"id": "c1", "latency": 100}
        });
        let (response, notification) = resp(handle_request(&req, &auth_config, &cmd_tx).await);
        assert_eq!(response["result"]["latency"], 100);
        let n = notification.expect("notification present");
        assert_eq!(n["method"], "Client.OnLatencyChanged");
        assert_eq!(n["params"]["latency"], 100);
        assert_eq!(n["params"]["id"], "c1");
    }

    #[tokio::test]
    async fn client_set_latency_defaults_to_zero() {
        let (auth_config, cmd_tx) = mock_server();
        // Missing `latency` -> defaults to 0.
        let req = json!({
            "jsonrpc": "2.0", "id": 28,
            "method": "Client.SetLatency", "params": {"id": "c1"}
        });
        let (response, _) = resp(handle_request(&req, &auth_config, &cmd_tx).await);
        assert_eq!(response["result"]["latency"], 0);
    }

    #[tokio::test]
    async fn client_set_name_happy_path() {
        let (auth_config, cmd_tx) = mock_server();
        let req = json!({
            "jsonrpc": "2.0", "id": 29,
            "method": "Client.SetName", "params": {"id": "c1", "name": "Kitchen"}
        });
        let (response, notification) = resp(handle_request(&req, &auth_config, &cmd_tx).await);
        assert_eq!(response["result"]["name"], "Kitchen");
        let n = notification.expect("notification present");
        assert_eq!(n["method"], "Client.OnNameChanged");
        assert_eq!(n["params"]["name"], "Kitchen");
    }

    #[tokio::test]
    async fn client_set_name_defaults_to_empty() {
        let (auth_config, cmd_tx) = mock_server();
        let req = json!({
            "jsonrpc": "2.0", "id": 30,
            "method": "Client.SetName", "params": {"id": "c1"}
        });
        let (response, _) = resp(handle_request(&req, &auth_config, &cmd_tx).await);
        assert_eq!(response["result"]["name"], "");
    }

    // --- Group.* ---------------------------------------------------------

    #[tokio::test]
    async fn group_get_status_found() {
        let (auth_config, cmd_tx) = mock_server();
        let req = json!({
            "jsonrpc": "2.0", "id": 40,
            "method": "Group.GetStatus", "params": {"id": "g1"}
        });
        let (response, _) = resp(handle_request(&req, &auth_config, &cmd_tx).await);
        assert_eq!(response["result"]["group"]["id"], "g1");
    }

    #[tokio::test]
    async fn group_get_status_not_found() {
        let (auth_config, cmd_tx) = mock_server();
        let req = json!({
            "jsonrpc": "2.0", "id": 41,
            "method": "Group.GetStatus", "params": {"id": "ghost"}
        });
        let (response, _) = resp(handle_request(&req, &auth_config, &cmd_tx).await);
        assert_eq!(response["error"]["message"], "group not found");
    }

    #[tokio::test]
    async fn group_set_mute_happy_path() {
        let (auth_config, cmd_tx) = mock_server();
        let req = json!({
            "jsonrpc": "2.0", "id": 42,
            "method": "Group.SetMute", "params": {"id": "g1", "mute": true}
        });
        let (response, notification) = resp(handle_request(&req, &auth_config, &cmd_tx).await);
        // Group uses the "mute" key (not "muted") on both result and notification.
        assert_eq!(response["result"]["mute"], true);
        let n = notification.expect("notification present");
        assert_eq!(n["method"], "Group.OnMute");
        assert_eq!(n["params"]["mute"], true);
        assert!(n["params"]["muted"].is_null());
    }

    #[tokio::test]
    async fn group_set_mute_defaults_false() {
        let (auth_config, cmd_tx) = mock_server();
        let req = json!({
            "jsonrpc": "2.0", "id": 43,
            "method": "Group.SetMute", "params": {"id": "g1"}
        });
        let (response, _) = resp(handle_request(&req, &auth_config, &cmd_tx).await);
        assert_eq!(response["result"]["mute"], false);
    }

    #[tokio::test]
    async fn group_set_stream_missing_stream_id() {
        let (auth_config, cmd_tx) = mock_server();
        let req = json!({
            "jsonrpc": "2.0", "id": 44,
            "method": "Group.SetStream", "params": {"id": "g1"}
        });
        let (response, notification) = resp(handle_request(&req, &auth_config, &cmd_tx).await);
        assert_eq!(response["error"]["message"], "missing 'stream_id'");
        assert!(notification.is_none());
    }

    #[tokio::test]
    async fn group_set_clients_happy_path_broadcasts_update() {
        let (auth_config, cmd_tx) = mock_server();
        let req = json!({
            "jsonrpc": "2.0", "id": 45,
            "method": "Group.SetClients",
            "params": {"id": "g1", "clients": ["c1", "c2"]}
        });
        let (response, notification) = resp(handle_request(&req, &auth_config, &cmd_tx).await);
        // Result is the fresh full status; notification is a Server.OnUpdate.
        assert!(response["result"]["server"]["groups"].is_array());
        let n = notification.expect("notification present");
        assert_eq!(n["method"], "Server.OnUpdate");
    }

    #[tokio::test]
    async fn group_set_clients_missing_array_is_invalid_params() {
        let (auth_config, cmd_tx) = mock_server();
        // `clients` is an object, not an array -> as_array() fails.
        let req = json!({
            "jsonrpc": "2.0", "id": 46,
            "method": "Group.SetClients",
            "params": {"id": "g1", "clients": {"not": "an array"}}
        });
        let (response, notification) = resp(handle_request(&req, &auth_config, &cmd_tx).await);
        assert_eq!(response["error"]["message"], "missing 'clients'");
        assert!(notification.is_none());
    }

    #[tokio::test]
    async fn group_set_name_happy_path() {
        let (auth_config, cmd_tx) = mock_server();
        let req = json!({
            "jsonrpc": "2.0", "id": 47,
            "method": "Group.SetName", "params": {"id": "g1", "name": "Main Room"}
        });
        let (response, notification) = resp(handle_request(&req, &auth_config, &cmd_tx).await);
        assert_eq!(response["result"]["name"], "Main Room");
        let n = notification.expect("notification present");
        assert_eq!(n["method"], "Group.OnNameChanged");
        assert_eq!(n["params"]["name"], "Main Room");
    }

    // --- Stream.* --------------------------------------------------------

    #[tokio::test]
    async fn stream_set_property_happy_path() {
        let (auth_config, cmd_tx) = mock_server();
        let req = json!({
            "jsonrpc": "2.0", "id": 50,
            "method": "Stream.SetProperty",
            "params": {"id": "default", "properties": {"artist": "Test"}}
        });
        let (response, notification) = resp(handle_request(&req, &auth_config, &cmd_tx).await);
        assert_eq!(response["result"]["id"], "default");
        assert_eq!(response["result"]["properties"]["artist"], "Test");
        let n = notification.expect("notification present");
        assert_eq!(n["method"], "Stream.OnUpdate");
        assert_eq!(n["params"]["properties"]["artist"], "Test");
    }

    #[tokio::test]
    async fn stream_set_property_missing_id() {
        let (auth_config, cmd_tx) = mock_server();
        let req = json!({
            "jsonrpc": "2.0", "id": 51,
            "method": "Stream.SetProperty", "params": {"properties": {}}
        });
        let (response, _) = resp(handle_request(&req, &auth_config, &cmd_tx).await);
        assert_eq!(response["error"]["message"], "missing 'id'");
    }

    #[tokio::test]
    async fn stream_control_happy_path() {
        let (auth_config, cmd_tx) = mock_server();
        let req = json!({
            "jsonrpc": "2.0", "id": 52,
            "method": "Stream.Control",
            "params": {"id": "default", "command": "next", "params": {}}
        });
        let (response, notification) = resp(handle_request(&req, &auth_config, &cmd_tx).await);
        assert_eq!(response["result"]["id"], "default");
        assert!(notification.is_none());
    }

    #[tokio::test]
    async fn stream_control_missing_command() {
        let (auth_config, cmd_tx) = mock_server();
        let req = json!({
            "jsonrpc": "2.0", "id": 53,
            "method": "Stream.Control", "params": {"id": "default"}
        });
        let (response, _) = resp(handle_request(&req, &auth_config, &cmd_tx).await);
        assert_eq!(response["error"]["message"], "missing 'command'");
    }

    #[tokio::test]
    async fn stream_add_stream_success() {
        let (auth_config, cmd_tx) = mock_server_addstream(Ok("stream-42".into()));
        let req = json!({
            "jsonrpc": "2.0", "id": 54,
            "method": "Stream.AddStream",
            "params": {"streamUri": "pipe:///tmp/snapfifo?name=default"}
        });
        let (response, notification) = resp(handle_request(&req, &auth_config, &cmd_tx).await);
        assert_eq!(response["result"]["id"], "stream-42");
        let n = notification.expect("notification present");
        assert_eq!(n["method"], "Server.OnUpdate");
    }

    #[tokio::test]
    async fn stream_add_stream_backend_error() {
        let (auth_config, cmd_tx) = mock_server_addstream(Err("bad uri".into()));
        let req = json!({
            "jsonrpc": "2.0", "id": 55,
            "method": "Stream.AddStream", "params": {"streamUri": "bogus://x"}
        });
        let (response, notification) = resp(handle_request(&req, &auth_config, &cmd_tx).await);
        assert_eq!(response["error"]["code"], INVALID_PARAMS);
        assert_eq!(response["error"]["message"], "bad uri");
        assert!(notification.is_none());
    }

    #[tokio::test]
    async fn stream_add_stream_missing_uri() {
        let (auth_config, cmd_tx) = mock_server_addstream(Ok("unused".into()));
        let req = json!({"jsonrpc": "2.0", "id": 56, "method": "Stream.AddStream", "params": {}});
        let (response, _) = resp(handle_request(&req, &auth_config, &cmd_tx).await);
        assert_eq!(response["error"]["message"], "missing 'streamUri'");
    }

    #[tokio::test]
    async fn stream_remove_stream_happy_path() {
        let (auth_config, cmd_tx) = mock_server();
        let req = json!({
            "jsonrpc": "2.0", "id": 57,
            "method": "Stream.RemoveStream", "params": {"id": "default"}
        });
        let (response, notification) = resp(handle_request(&req, &auth_config, &cmd_tx).await);
        assert_eq!(response["result"]["id"], "default");
        let n = notification.expect("notification present");
        assert_eq!(n["method"], "Server.OnUpdate");
    }

    // --- Auth ------------------------------------------------------------

    #[tokio::test]
    async fn server_get_token_with_enabled_auth() {
        let (auth_config, cmd_tx) = auth_enabled();
        let req = json!({
            "jsonrpc": "2.0", "id": 60,
            "method": "Server.GetToken", "params": {"username": "bob"}
        });
        let (response, _) = resp(handle_request(&req, &auth_config, &cmd_tx).await);
        let token = response["result"]["token"].as_str().expect("token issued");
        assert!(!token.is_empty());
        // Round-trip: the issued token validates back to the requested subject.
        assert_eq!(auth::validate_token(&auth_config, token).unwrap(), "bob");
    }

    #[tokio::test]
    async fn server_get_token_defaults_to_anonymous() {
        let (auth_config, cmd_tx) = auth_enabled();
        let req = json!({"jsonrpc": "2.0", "id": 61, "method": "Server.GetToken", "params": {}});
        let (response, _) = resp(handle_request(&req, &auth_config, &cmd_tx).await);
        let token = response["result"]["token"].as_str().expect("token issued");
        assert_eq!(
            auth::validate_token(&auth_config, token).unwrap(),
            "anonymous"
        );
    }

    #[tokio::test]
    async fn server_get_token_fails_without_secret() {
        // Default AuthConfig has an empty secret -> generate_token errors.
        let (auth_config, cmd_tx) = mock_server();
        let req = json!({
            "jsonrpc": "2.0", "id": 62,
            "method": "Server.GetToken", "params": {"username": "bob"}
        });
        let (response, _) = resp(handle_request(&req, &auth_config, &cmd_tx).await);
        assert_eq!(response["error"]["code"], INVALID_PARAMS);
        assert!(
            response["error"]["message"]
                .as_str()
                .unwrap()
                .contains("token generation failed")
        );
    }

    #[tokio::test]
    async fn server_authenticate_valid_token() {
        let (auth_config, cmd_tx) = auth_enabled();
        let token = auth::generate_token(&auth_config, "alice").unwrap();
        let req = json!({
            "jsonrpc": "2.0", "id": 63,
            "method": "Server.Authenticate", "params": {"token": token}
        });
        let (response, _) = resp(handle_request(&req, &auth_config, &cmd_tx).await);
        assert_eq!(response["result"]["ok"], true);
        assert_eq!(response["result"]["subject"], "alice");
    }

    #[tokio::test]
    async fn server_authenticate_invalid_token() {
        let (auth_config, cmd_tx) = auth_enabled();
        let req = json!({
            "jsonrpc": "2.0", "id": 64,
            "method": "Server.Authenticate", "params": {"token": "not.a.jwt"}
        });
        let (response, _) = resp(handle_request(&req, &auth_config, &cmd_tx).await);
        assert_eq!(response["error"]["code"], INVALID_PARAMS);
        assert_eq!(response["error"]["message"], "invalid token");
    }

    #[tokio::test]
    async fn server_authenticate_missing_token() {
        let (auth_config, cmd_tx) = auth_enabled();
        let req =
            json!({"jsonrpc": "2.0", "id": 65, "method": "Server.Authenticate", "params": {}});
        let (response, _) = resp(handle_request(&req, &auth_config, &cmd_tx).await);
        assert_eq!(response["error"]["message"], "missing 'token'");
    }

    // --- Dispatch edge cases --------------------------------------------

    #[tokio::test]
    async fn empty_method_is_unknown() {
        let (auth_config, cmd_tx) = mock_server();
        // No `method` field at all: `.as_str().unwrap_or("")` -> "" -> Unknown.
        let req = json!({"jsonrpc": "2.0", "id": 70, "params": {}});
        assert!(matches!(
            handle_request(&req, &auth_config, &cmd_tx).await,
            RpcResult::Unknown
        ));
    }

    #[tokio::test]
    async fn non_string_method_is_unknown() {
        let (auth_config, cmd_tx) = mock_server();
        // `method` present but wrong type (number): as_str() -> None -> "".
        let req = json!({"jsonrpc": "2.0", "id": 71, "method": 12345, "params": {}});
        assert!(matches!(
            handle_request(&req, &auth_config, &cmd_tx).await,
            RpcResult::Unknown
        ));
    }

    #[tokio::test]
    async fn string_id_is_echoed_in_response() {
        let (auth_config, cmd_tx) = mock_server();
        let req = json!({
            "jsonrpc": "2.0", "id": "req-abc",
            "method": "Server.GetRPCVersion"
        });
        let (response, _) = resp(handle_request(&req, &auth_config, &cmd_tx).await);
        assert_eq!(response["id"], "req-abc");
    }

    #[tokio::test]
    async fn null_id_error_response_still_echoes_null() {
        let (auth_config, cmd_tx) = mock_server();
        // Missing id -> Value::Null; an error response must still carry it.
        let req = json!({"method": "Server.DeleteClient", "params": {}});
        let (response, _) = resp(handle_request(&req, &auth_config, &cmd_tx).await);
        assert!(response["id"].is_null());
        assert_eq!(response["error"]["message"], "missing 'id'");
    }
}
