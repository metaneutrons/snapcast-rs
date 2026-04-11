//! JSON-RPC control API — method handlers for Snapcast control protocol.

use serde_json::{Value, json};

use crate::auth::{self, AuthConfig};

/// JSON-RPC error codes.
const INVALID_PARAMS: i64 = -32602;

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
    Some(json!({"server": {"groups": status.groups, "streams": status.streams}}))
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
            let status = get_status(cmd_tx).await.unwrap_or_default();
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

        // --- Stream ---
        "Stream.SetProperty" => {
            let Some(stream_id) = params["id"].as_str() else {
                return err(id, INVALID_PARAMS, "missing 'id'");
            };
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
            let Some(stream_id) = params["id"].as_str() else {
                return err(id, INVALID_PARAMS, "missing 'id'");
            };
            let Some(command) = params["command"].as_str() else {
                return err(id, INVALID_PARAMS, "missing 'command'");
            };
            tracing::debug!(stream_id, command, "Stream control");
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
                            groups: vec![status::GroupStatus {
                                id: "g1".into(),
                                name: String::new(),
                                stream_id: "default".into(),
                                muted: false,
                                clients: vec![status::ClientStatus {
                                    id: "c1".into(),
                                    connected: true,
                                    config: status::ClientConfig {
                                        name: String::new(),
                                        volume: status::VolumeInfo {
                                            percent: volume,
                                            muted,
                                        },
                                        latency: 0,
                                    },
                                    host: status::HostInfo {
                                        name: "host1".into(),
                                        mac: "mac1".into(),
                                    },
                                }],
                            }],
                            streams: vec![status::StreamStatus {
                                id: "default".into(),
                                status: "playing".into(),
                                uri: String::new(),
                                properties: Default::default(),
                            }],
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
}
