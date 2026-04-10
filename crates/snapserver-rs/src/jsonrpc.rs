//! JSON-RPC control API — method handlers for Snapcast control protocol.

use std::sync::Arc;

use serde_json::{Value, json};
use tokio::sync::Mutex;

use crate::auth::{self, AuthConfig};
use snapcast_server::state::ServerState;

/// JSON-RPC error codes.
const INVALID_PARAMS: i64 = -32602;

/// Result of handling a JSON-RPC request.
pub enum RpcResult {
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

/// Handle a JSON-RPC request against shared server state.
pub async fn handle_request(
    request: &Value,
    state: &Arc<Mutex<ServerState>>,
    auth_config: &AuthConfig,
    cmd_tx: &tokio::sync::mpsc::Sender<snapcast_server::ServerCommand>,
) -> RpcResult {
    let id = &request["id"];
    let method = request["method"].as_str().unwrap_or("");
    let params = &request["params"];

    match method {
        // --- Server ---
        "Server.GetRPCVersion" => ok(id, json!({"major": 2, "minor": 0, "patch": 0})),
        "Server.GetStatus" => {
            let (tx, rx) = tokio::sync::oneshot::channel();
            let _ = cmd_tx
                .send(snapcast_server::ServerCommand::GetStatus { response_tx: tx })
                .await;
            match rx.await {
                Ok(status) => ok(id, status),
                Err(_) => err(id, INVALID_PARAMS, "status unavailable"),
            }
        }
        "Server.DeleteClient" => {
            let Some(client_id) = params["id"].as_str() else {
                return err(id, INVALID_PARAMS, "missing 'id'");
            };
            let _ = cmd_tx
                .send(snapcast_server::ServerCommand::DeleteClient {
                    client_id: client_id.to_string(),
                })
                .await;
            ok(id, json!({"id": client_id}))
        }

        // --- Client ---
        "Client.GetStatus" => {
            let Some(client_id) = params["id"].as_str() else {
                return err(id, INVALID_PARAMS, "missing 'id'");
            };
            let s = state.lock().await;
            match s.clients.get(client_id) {
                Some(c) => ok(id, json!({"client": client_json(c)})),
                None => err(id, INVALID_PARAMS, "client not found"),
            }
        }
        "Client.SetVolume" => {
            let Some(client_id) = params["id"].as_str() else {
                return err(id, INVALID_PARAMS, "missing 'id'");
            };
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
            ok_with_notify(
                id,
                json!({"volume": &vol}),
                "Client.OnVolumeChanged",
                json!({"id": client_id, "volume": vol}),
            )
        }
        "Client.SetLatency" => {
            let Some(client_id) = params["id"].as_str() else {
                return err(id, INVALID_PARAMS, "missing 'id'");
            };
            let latency = params["latency"].as_i64().unwrap_or(0) as i32;
            let _ = cmd_tx
                .send(snapcast_server::ServerCommand::SetClientLatency {
                    client_id: client_id.to_string(),
                    latency,
                })
                .await;
            ok_with_notify(
                id,
                json!({"latency": latency}),
                "Client.OnLatencyChanged",
                json!({"id": client_id, "latency": latency}),
            )
        }
        "Client.SetName" => {
            let Some(client_id) = params["id"].as_str() else {
                return err(id, INVALID_PARAMS, "missing 'id'");
            };
            let name = params["name"].as_str().unwrap_or("").to_string();
            let _ = cmd_tx
                .send(snapcast_server::ServerCommand::SetClientName {
                    client_id: client_id.to_string(),
                    name: name.clone(),
                })
                .await;
            ok_with_notify(
                id,
                json!({"name": &name}),
                "Client.OnNameChanged",
                json!({"id": client_id, "name": name}),
            )
        }

        // --- Group ---
        "Group.GetStatus" => {
            let Some(group_id) = params["id"].as_str() else {
                return err(id, INVALID_PARAMS, "missing 'id'");
            };
            let s = state.lock().await;
            match s.groups.iter().find(|g| g.id == group_id) {
                Some(g) => ok(id, json!({"group": group_json(g, &s)})),
                None => err(id, INVALID_PARAMS, "group not found"),
            }
        }
        "Group.SetMute" => {
            let Some(group_id) = params["id"].as_str() else {
                return err(id, INVALID_PARAMS, "missing 'id'");
            };
            let muted = params["mute"].as_bool().unwrap_or(false);
            let _ = cmd_tx
                .send(snapcast_server::ServerCommand::SetGroupMute {
                    group_id: group_id.to_string(),
                    muted,
                })
                .await;
            ok_with_notify(
                id,
                json!({"mute": muted}),
                "Group.OnMute",
                json!({"id": group_id, "mute": muted}),
            )
        }
        "Group.SetStream" => {
            let Some(group_id) = params["id"].as_str() else {
                return err(id, INVALID_PARAMS, "missing 'id'");
            };
            let Some(stream_id) = params["stream_id"].as_str() else {
                return err(id, INVALID_PARAMS, "missing 'stream_id'");
            };
            let _ = cmd_tx
                .send(snapcast_server::ServerCommand::SetGroupStream {
                    group_id: group_id.to_string(),
                    stream_id: stream_id.to_string(),
                })
                .await;
            ok_with_notify(
                id,
                json!({"stream_id": stream_id}),
                "Group.OnStreamChanged",
                json!({"id": group_id, "stream_id": stream_id}),
            )
        }
        "Group.SetClients" => {
            let Some(group_id) = params["id"].as_str() else {
                return err(id, INVALID_PARAMS, "missing 'id'");
            };
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
            // Return full status after group change
            let (tx, rx) = tokio::sync::oneshot::channel();
            let _ = cmd_tx
                .send(snapcast_server::ServerCommand::GetStatus { response_tx: tx })
                .await;
            let status = rx.await.unwrap_or_default();
            ok_with_notify(id, status.clone(), "Server.OnUpdate", status)
        }
        "Group.SetName" => {
            let Some(group_id) = params["id"].as_str() else {
                return err(id, INVALID_PARAMS, "missing 'id'");
            };
            let name = params["name"].as_str().unwrap_or("").to_string();
            let _ = cmd_tx
                .send(snapcast_server::ServerCommand::SetGroupName {
                    group_id: group_id.to_string(),
                    name: name.clone(),
                })
                .await;
            ok_with_notify(
                id,
                json!({"name": &name}),
                "Group.OnNameChanged",
                json!({"id": group_id, "name": name}),
            )
        }
        "Stream.SetProperty" => {
            let Some(stream_id) = params["id"].as_str() else {
                return err(id, INVALID_PARAMS, "missing 'id'");
            };
            let mut s = state.lock().await;
            if let Some(stream) = s.streams.iter_mut().find(|st| st.id == stream_id) {
                if let Some(props) = params["properties"].as_object() {
                    for (k, v) in props {
                        stream.properties.insert(k.clone(), v.clone());
                    }
                }
                let props: Value = stream.properties.clone().into_iter().collect();
                ok_with_notify(
                    id,
                    json!({"id": stream_id, "properties": &props}),
                    "Stream.OnUpdate",
                    json!({"id": stream_id, "properties": props}),
                )
            } else {
                err(id, INVALID_PARAMS, "stream not found")
            }
        }
        "Stream.Control" => {
            let Some(stream_id) = params["id"].as_str() else {
                return err(id, INVALID_PARAMS, "missing 'id'");
            };
            let Some(command) = params["command"].as_str() else {
                return err(id, INVALID_PARAMS, "missing 'command'");
            };
            let msg_stream_id = stream_id.to_string();
            let msg_command = command.to_string();
            tracing::debug!(stream_id = %msg_stream_id, command = %msg_command, "Stream control");
            ok(id, json!({"id": stream_id}))
        }

        // --- Auth ---
        "Server.GetToken" => {
            let username = params["username"].as_str().unwrap_or("anonymous");
            match auth::generate_token(auth_config, username) {
                Ok(token) => ok(id, json!({"token": token})),
                Err(e) => err(id, INVALID_PARAMS, &format!("token generation failed: {e}")),
            }
        }
        "Server.Authenticate" => {
            let Some(token) = params["token"].as_str() else {
                return err(id, INVALID_PARAMS, "missing 'token'");
            };
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

fn err(id: &Value, code: i64, msg: &str) -> RpcResult {
    RpcResult::Response {
        response: json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": msg}}),
        notification: None,
    }
}

fn client_json(c: &snapcast_server::state::Client) -> Value {
    json!({
        "id": c.id,
        "host": {"name": c.host_name, "mac": c.mac},
        "connected": c.connected,
        "config": {
            "name": c.config.name,
            "volume": {"percent": c.config.volume.percent, "muted": c.config.volume.muted},
            "latency": c.config.latency,
        }
    })
}

fn group_json(g: &snapcast_server::state::Group, s: &snapcast_server::state::ServerState) -> Value {
    let clients: Vec<Value> = g
        .clients
        .iter()
        .filter_map(|cid| s.clients.get(cid))
        .map(client_json)
        .collect();
    json!({
        "id": g.id,
        "name": g.name,
        "stream_id": g.stream_id,
        "muted": g.muted,
        "clients": clients,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_state() -> (
        Arc<Mutex<ServerState>>,
        AuthConfig,
        tokio::sync::mpsc::Sender<snapcast_server::ServerCommand>,
    ) {
        let mut s = ServerState::default();
        s.get_or_create_client("c1", "host1", "mac1");
        s.clients.get_mut("c1").unwrap().connected = true;
        s.group_for_client("c1", "default");
        s.streams.push(snapcast_server::state::StreamInfo {
            id: "default".into(),
            status: "playing".into(),
            uri: "pipe:///tmp/snapfifo".into(),
            properties: Default::default(),
        });
        let state = Arc::new(Mutex::new(s));
        let (cmd_tx, mut cmd_rx) = tokio::sync::mpsc::channel(16);
        let state_for_cmd = Arc::clone(&state);
        tokio::spawn(async move {
            while let Some(cmd) = cmd_rx.recv().await {
                match cmd {
                    snapcast_server::ServerCommand::GetStatus { response_tx } => {
                        let s = state_for_cmd.lock().await;
                        let _ = response_tx.send(s.to_status_json());
                    }
                    snapcast_server::ServerCommand::SetClientVolume {
                        client_id,
                        volume,
                        muted,
                    } => {
                        let mut s = state_for_cmd.lock().await;
                        if let Some(c) = s.clients.get_mut(&client_id) {
                            c.config.volume.percent = volume;
                            c.config.volume.muted = muted;
                        }
                    }
                    _ => {}
                }
            }
        });
        (state, AuthConfig::default(), cmd_tx)
    }

    #[tokio::test]
    async fn server_get_status() {
        let (state, auth_config, cmd_tx) = test_state().await;
        let req = json!({"jsonrpc": "2.0", "id": 1, "method": "Server.GetStatus", "params": {}});
        let RpcResult::Response { response, .. } =
            handle_request(&req, &state, &auth_config, &cmd_tx).await
        else {
            panic!("expected response");
        };
        assert!(response["result"]["server"]["groups"].is_array());
    }

    #[tokio::test]
    async fn client_set_volume() {
        let (state, auth_config, cmd_tx) = test_state().await;
        let req = json!({
            "jsonrpc": "2.0", "id": 2,
            "method": "Client.SetVolume",
            "params": {"id": "c1", "volume": {"percent": 50, "muted": true}}
        });
        let RpcResult::Response {
            response,
            notification,
        } = handle_request(&req, &state, &auth_config, &cmd_tx).await
        else {
            panic!("expected response");
        };
        assert_eq!(response["result"]["volume"]["percent"], 50);
        assert!(notification.is_some());
        assert_eq!(notification.unwrap()["method"], "Client.OnVolumeChanged");

        // Let the command handler process the SetClientVolume
        tokio::task::yield_now().await;

        let s = state.lock().await;
        assert_eq!(s.clients["c1"].config.volume.percent, 50);
        assert!(s.clients["c1"].config.volume.muted);
    }

    #[tokio::test]
    async fn unknown_method_returns_unknown() {
        let (state, auth_config, cmd_tx) = test_state().await;
        let req = json!({"jsonrpc": "2.0", "id": 3, "method": "Client.SetEq", "params": {}});
        assert!(matches!(
            handle_request(&req, &state, &auth_config, &cmd_tx).await,
            RpcResult::Unknown
        ));
    }

    #[tokio::test]
    async fn group_set_stream() {
        let (state, auth_config, cmd_tx) = test_state().await;
        let gid = {
            let s = state.lock().await;
            s.groups[0].id.clone()
        };
        let req = json!({
            "jsonrpc": "2.0", "id": 4,
            "method": "Group.SetStream",
            "params": {"id": gid, "stream_id": "music"}
        });
        let RpcResult::Response { notification, .. } =
            handle_request(&req, &state, &auth_config, &cmd_tx).await
        else {
            panic!("expected response");
        };
        assert_eq!(notification.unwrap()["params"]["stream_id"], "music");
    }
}
