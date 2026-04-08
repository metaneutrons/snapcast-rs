//! Server state model — clients, groups, streams with JSON persistence.

use std::collections::HashMap;
use std::path::Path;

use anyhow::Result;
use serde::{Deserialize, Serialize};

/// Volume settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Volume {
    /// Volume percentage (0–100).
    pub percent: u16,
    /// Mute state.
    pub muted: bool,
}

impl Default for Volume {
    fn default() -> Self {
        Self {
            percent: 100,
            muted: false,
        }
    }
}

/// Client configuration (persisted).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ClientConfig {
    /// Display name (user-assigned).
    #[serde(default)]
    pub name: String,
    /// Volume.
    #[serde(default)]
    pub volume: Volume,
    /// Additional latency in milliseconds.
    #[serde(default)]
    pub latency: i32,
}

/// A connected or previously-seen client.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Client {
    /// Unique client ID.
    pub id: String,
    /// Hostname.
    pub host_name: String,
    /// MAC address.
    pub mac: String,
    /// Whether currently connected.
    pub connected: bool,
    /// Client configuration.
    pub config: ClientConfig,
}

/// A group of clients sharing the same stream.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Group {
    /// Unique group ID.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Stream ID this group is playing.
    pub stream_id: String,
    /// Group mute state.
    pub muted: bool,
    /// Client IDs in this group.
    pub clients: Vec<String>,
}

/// A stream source.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamInfo {
    /// Stream ID (= name from URI).
    pub id: String,
    /// Status: "playing", "idle", "unknown".
    pub status: String,
    /// Source URI.
    pub uri: String,
    /// Stream properties (metadata: artist, title, etc.).
    #[serde(default)]
    pub properties: std::collections::HashMap<String, serde_json::Value>,
}

/// Complete server state.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ServerState {
    /// All known clients (by ID).
    pub clients: HashMap<String, Client>,
    /// All groups.
    pub groups: Vec<Group>,
    /// All streams.
    pub streams: Vec<StreamInfo>,
}

impl ServerState {
    /// Load state from a JSON file, or return default if not found.
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// Save state to a JSON file.
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(path, json)?;
        Ok(())
    }

    /// Get or create a client entry. Returns mutable reference.
    pub fn get_or_create_client(&mut self, id: &str, host_name: &str, mac: &str) -> &mut Client {
        if !self.clients.contains_key(id) {
            self.clients.insert(
                id.to_string(),
                Client {
                    id: id.to_string(),
                    host_name: host_name.to_string(),
                    mac: mac.to_string(),
                    connected: false,
                    config: ClientConfig::default(),
                },
            );
        }
        self.clients.get_mut(id).unwrap()
    }

    /// Find which group a client belongs to, or create a new group.
    pub fn group_for_client(&mut self, client_id: &str, default_stream: &str) -> &mut Group {
        // Check if client is already in a group
        let idx = self
            .groups
            .iter()
            .position(|g| g.clients.contains(&client_id.to_string()));

        if let Some(idx) = idx {
            return &mut self.groups[idx];
        }

        // Create new group with this client
        let group = Group {
            id: uuid_v4(),
            name: String::new(),
            stream_id: default_stream.to_string(),
            muted: false,
            clients: vec![client_id.to_string()],
        };
        self.groups.push(group);
        self.groups.last_mut().unwrap()
    }

    /// Remove a client from all groups.
    pub fn remove_client_from_groups(&mut self, client_id: &str) {
        for group in &mut self.groups {
            group.clients.retain(|c| c != client_id);
        }
        // Remove empty groups
        self.groups.retain(|g| !g.clients.is_empty());
    }

    /// Move a client to a different group.
    pub fn move_client_to_group(&mut self, client_id: &str, group_id: &str) {
        self.remove_client_from_groups(client_id);
        if let Some(group) = self.groups.iter_mut().find(|g| g.id == group_id) {
            group.clients.push(client_id.to_string());
        }
    }

    /// Set a group's stream.
    pub fn set_group_stream(&mut self, group_id: &str, stream_id: &str) {
        if let Some(group) = self.groups.iter_mut().find(|g| g.id == group_id) {
            group.stream_id = stream_id.to_string();
        }
    }

    /// Build JSON status for the full server (matches C++ Server.GetStatus).
    pub fn to_status_json(&self) -> serde_json::Value {
        let groups: Vec<serde_json::Value> = self
            .groups
            .iter()
            .map(|g| {
                let clients: Vec<serde_json::Value> = g
                    .clients
                    .iter()
                    .filter_map(|cid| self.clients.get(cid))
                    .map(|c| {
                        serde_json::json!({
                            "id": c.id,
                            "host": { "name": c.host_name, "mac": c.mac },
                            "connected": c.connected,
                            "config": {
                                "name": c.config.name,
                                "volume": { "percent": c.config.volume.percent, "muted": c.config.volume.muted },
                                "latency": c.config.latency,
                            }
                        })
                    })
                    .collect();
                serde_json::json!({
                    "id": g.id,
                    "name": g.name,
                    "stream_id": g.stream_id,
                    "muted": g.muted,
                    "clients": clients,
                })
            })
            .collect();

        let streams: Vec<serde_json::Value> = self
            .streams
            .iter()
            .map(|s| {
                serde_json::json!({
                    "id": s.id,
                    "status": s.status,
                    "uri": { "raw": s.uri },
                })
            })
            .collect();

        serde_json::json!({
            "server": {
                "groups": groups,
                "streams": streams,
            }
        })
    }
}

fn uuid_v4() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{t:032x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_lifecycle() {
        let mut state = ServerState::default();
        let c = state.get_or_create_client("abc", "myhost", "aa:bb:cc:dd:ee:ff");
        assert_eq!(c.id, "abc");
        assert_eq!(c.config.volume.percent, 100);

        let g = state.group_for_client("abc", "default");
        assert_eq!(g.clients, vec!["abc"]);
        let gid = g.id.clone();

        // Same client → same group
        let g2 = state.group_for_client("abc", "default");
        assert_eq!(g2.id, gid);
    }

    #[test]
    fn move_client() {
        let mut state = ServerState::default();
        state.get_or_create_client("c1", "h1", "m1");
        state.get_or_create_client("c2", "h2", "m2");
        state.group_for_client("c1", "s1");
        state.group_for_client("c2", "s1");

        assert_eq!(state.groups.len(), 2);

        let g2_id = state.groups[1].id.clone();
        state.move_client_to_group("c1", &g2_id);

        assert_eq!(state.groups.len(), 1); // empty group removed
        assert_eq!(state.groups[0].clients.len(), 2);
    }

    #[test]
    fn json_roundtrip() {
        let mut state = ServerState::default();
        state.get_or_create_client("c1", "host1", "mac1");
        state.group_for_client("c1", "default");
        state.streams.push(StreamInfo {
            id: "default".into(),
            status: "playing".into(),
            uri: "pipe:///tmp/snapfifo".into(),
            properties: Default::default(),
        });

        let json = serde_json::to_string(&state).unwrap();
        let restored: ServerState = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.clients.len(), 1);
        assert_eq!(restored.groups.len(), 1);
        assert_eq!(restored.streams.len(), 1);
    }

    #[test]
    fn status_json() {
        let mut state = ServerState::default();
        state.get_or_create_client("c1", "host1", "mac1");
        state.group_for_client("c1", "default");
        let status = state.to_status_json();
        assert!(status["server"]["groups"].is_array());
    }
}
