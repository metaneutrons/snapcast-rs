//! JSON-RPC control API — method handlers for Snapcast control protocol.

use std::sync::Arc;

use serde_json::{Value, json};
use tokio::sync::Mutex;

use crate::auth::{self, AuthConfig};
use crate::state::ServerState;

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
/// A control command sent to a stream reader.
#[derive(Debug, Clone)]
pub struct StreamControlMsg {
    /// Target stream ID.
    pub stream_id: String,
    /// Command (e.g. "play", "pause", "next").
    pub command: String,
    /// Command parameters.
    pub params: Value,
}

/// A settings update to push to a streaming client via binary protocol.
#[derive(Debug, Clone)]
pub struct ClientSettingsUpdate {
    /// Target client ID.
    pub client_id: String,
    /// Buffer size in ms.
    pub buffer_ms: i32,
    /// Latency offset in ms.
    pub latency: i32,
    /// Volume (0–100).
    pub volume: u16,
    /// Mute state.
    pub muted: bool,
}

/// Handle a JSON-RPC request against shared server state.
pub async fn handle_request(
    request: &Value,
    state: &Arc<Mutex<ServerState>>,
    auth_config: &AuthConfig,
    stream_control_tx: &tokio::sync::mpsc::Sender<StreamControlMsg>,
    settings_tx: &tokio::sync::mpsc::Sender<ClientSettingsUpdate>,
) -> RpcResult {
    let id = &request["id"];
    let method = request["method"].as_str().unwrap_or("");
    let params = &request["params"];

    match method {
        // --- Server ---
        "Server.GetRPCVersion" => ok(id, json!({"major": 2, "minor": 0, "patch": 0})),
        "Server.GetStatus" => {
            let s = state.lock().await;
            ok(id, s.to_status_json())
        }
        "Server.DeleteClient" => {
            let Some(client_id) = params["id"].as_str() else {
                return err(id, INVALID_PARAMS, "missing 'id'");
            };
            let mut s = state.lock().await;
            s.remove_client_from_groups(client_id);
            s.clients.remove(client_id);
            ok_with_notify(
                id,
                json!({"id": client_id}),
                "Server.OnUpdate",
                s.to_status_json(),
            )
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
            let mut s = state.lock().await;
            let Some(c) = s.clients.get_mut(client_id) else {
                return err(id, INVALID_PARAMS, "client not found");
            };
            if let Some(vol) = params["volume"]["percent"].as_u64() {
                c.config.volume.percent = vol.min(100) as u16;
            }
            if let Some(muted) = params["volume"]["muted"].as_bool() {
                c.config.volume.muted = muted;
            }
            let vol = json!({"percent": c.config.volume.percent, "muted": c.config.volume.muted});
            // Push to streaming client via binary protocol
            let _ = settings_tx.send(client_settings_update(client_id, c)).await;
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
            let mut s = state.lock().await;
            let Some(c) = s.clients.get_mut(client_id) else {
                return err(id, INVALID_PARAMS, "client not found");
            };
            if let Some(lat) = params["latency"].as_i64() {
                c.config.latency = lat as i32;
            }
            // Push to streaming client via binary protocol
            let _ = settings_tx.send(client_settings_update(client_id, c)).await;
            ok_with_notify(
                id,
                json!({"latency": c.config.latency}),
                "Client.OnLatencyChanged",
                json!({"id": client_id, "latency": c.config.latency}),
            )
        }
        "Client.SetName" => {
            let Some(client_id) = params["id"].as_str() else {
                return err(id, INVALID_PARAMS, "missing 'id'");
            };
            let mut s = state.lock().await;
            let Some(c) = s.clients.get_mut(client_id) else {
                return err(id, INVALID_PARAMS, "client not found");
            };
            if let Some(name) = params["name"].as_str() {
                c.config.name = name.to_string();
            }
            ok_with_notify(
                id,
                json!({"name": &c.config.name}),
                "Client.OnNameChanged",
                json!({"id": client_id, "name": &c.config.name}),
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
            let mut s = state.lock().await;
            let Some(g) = s.groups.iter_mut().find(|g| g.id == group_id) else {
                return err(id, INVALID_PARAMS, "group not found");
            };
            if let Some(mute) = params["mute"].as_bool() {
                g.muted = mute;
            }
            ok_with_notify(
                id,
                json!({"mute": g.muted}),
                "Group.OnMute",
                json!({"id": group_id, "mute": g.muted}),
            )
        }
        "Group.SetStream" => {
            let Some(group_id) = params["id"].as_str() else {
                return err(id, INVALID_PARAMS, "missing 'id'");
            };
            let Some(stream_id) = params["stream_id"].as_str() else {
                return err(id, INVALID_PARAMS, "missing 'stream_id'");
            };
            let mut s = state.lock().await;
            s.set_group_stream(group_id, stream_id);
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
            let mut s = state.lock().await;
            // Remove clients from other groups, add to target
            for cid in &client_ids {
                s.remove_client_from_groups(cid);
            }
            if let Some(g) = s.groups.iter_mut().find(|g| g.id == group_id) {
                g.clients = client_ids;
            }
            ok_with_notify(
                id,
                s.to_status_json(),
                "Server.OnUpdate",
                s.to_status_json(),
            )
        }
        "Group.SetName" => {
            let Some(group_id) = params["id"].as_str() else {
                return err(id, INVALID_PARAMS, "missing 'id'");
            };
            let mut s = state.lock().await;
            let Some(g) = s.groups.iter_mut().find(|g| g.id == group_id) else {
                return err(id, INVALID_PARAMS, "group not found");
            };
            if let Some(name) = params["name"].as_str() {
                g.name = name.to_string();
            }
            ok_with_notify(
                id,
                json!({"name": &g.name}),
                "Group.OnNameChanged",
                json!({"id": group_id, "name": &g.name}),
            )
        }

        // --- Stream ---
        "Stream.AddStream" => {
            let Some(uri) = params["streamUri"].as_str() else {
                return err(id, INVALID_PARAMS, "missing 'streamUri'");
            };
            let s = state.lock().await;
            // Stream creation is handled by StreamManager, not state.
            // For now, just acknowledge.
            ok_with_notify(
                id,
                json!({"id": uri}),
                "Server.OnUpdate",
                s.to_status_json(),
            )
        }
        "Stream.RemoveStream" => {
            let Some(stream_id) = params["id"].as_str() else {
                return err(id, INVALID_PARAMS, "missing 'id'");
            };
            let mut s = state.lock().await;
            s.streams.retain(|st| st.id != stream_id);
            ok_with_notify(
                id,
                json!({"id": stream_id}),
                "Server.OnUpdate",
                s.to_status_json(),
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
            let msg = StreamControlMsg {
                stream_id: stream_id.to_string(),
                command: command.to_string(),
                params: params["params"].clone(),
            };
            let _ = stream_control_tx.send(msg).await;
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

fn client_settings_update(client_id: &str, c: &crate::state::Client) -> ClientSettingsUpdate {
    ClientSettingsUpdate {
        client_id: client_id.to_string(),
        buffer_ms: 1000, // TODO: from server config
        latency: c.config.latency,
        volume: c.config.volume.percent,
        muted: c.config.volume.muted,
    }
}
fn client_json(c: &crate::state::Client) -> Value {
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

fn group_json(g: &crate::state::Group, s: &crate::state::ServerState) -> Value {
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
        tokio::sync::mpsc::Sender<StreamControlMsg>,
        tokio::sync::mpsc::Sender<ClientSettingsUpdate>,
    ) {
        let mut s = ServerState::default();
        s.get_or_create_client("c1", "host1", "mac1");
        s.clients.get_mut("c1").unwrap().connected = true;
        s.group_for_client("c1", "default");
        s.streams.push(crate::state::StreamInfo {
            id: "default".into(),
            status: "playing".into(),
            uri: "pipe:///tmp/snapfifo".into(),
            properties: Default::default(),
        });
        let (tx, _rx) = tokio::sync::mpsc::channel(16);
        let (stx, _srx) = tokio::sync::mpsc::channel(16);
        (Arc::new(Mutex::new(s)), AuthConfig::default(), tx, stx)
    }

    #[tokio::test]
    async fn server_get_status() {
        let (state, auth_config, stream_tx, settings_tx) = test_state().await;
        let req = json!({"jsonrpc": "2.0", "id": 1, "method": "Server.GetStatus", "params": {}});
        let RpcResult::Response { response, .. } =
            handle_request(&req, &state, &auth_config, &stream_tx, &settings_tx).await
        else {
            panic!("expected response");
        };
        assert!(response["result"]["server"]["groups"].is_array());
    }

    #[tokio::test]
    async fn client_set_volume() {
        let (state, auth_config, stream_tx, settings_tx) = test_state().await;
        let req = json!({
            "jsonrpc": "2.0", "id": 2,
            "method": "Client.SetVolume",
            "params": {"id": "c1", "volume": {"percent": 50, "muted": true}}
        });
        let RpcResult::Response {
            response,
            notification,
        } = handle_request(&req, &state, &auth_config, &stream_tx, &settings_tx).await
        else {
            panic!("expected response");
        };
        assert_eq!(response["result"]["volume"]["percent"], 50);
        assert!(notification.is_some());
        assert_eq!(notification.unwrap()["method"], "Client.OnVolumeChanged");

        let s = state.lock().await;
        assert_eq!(s.clients["c1"].config.volume.percent, 50);
        assert!(s.clients["c1"].config.volume.muted);
    }

    #[tokio::test]
    async fn unknown_method_returns_unknown() {
        let (state, auth_config, stream_tx, settings_tx) = test_state().await;
        let req = json!({"jsonrpc": "2.0", "id": 3, "method": "Client.SetEq", "params": {}});
        assert!(matches!(
            handle_request(&req, &state, &auth_config, &stream_tx, &settings_tx).await,
            RpcResult::Unknown
        ));
    }

    #[tokio::test]
    async fn group_set_stream() {
        let (state, auth_config, stream_tx, settings_tx) = test_state().await;
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
            handle_request(&req, &state, &auth_config, &stream_tx, &settings_tx).await
        else {
            panic!("expected response");
        };
        assert_eq!(notification.unwrap()["params"]["stream_id"], "music");
    }
}
