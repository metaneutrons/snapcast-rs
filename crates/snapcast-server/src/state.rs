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
        let state: Self = std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        tracing::debug!(path = %path.display(), clients = state.clients.len(), groups = state.groups.len(), "state loaded");
        state
    }

    /// Save state to a JSON file.
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(path, json)?;
        tracing::debug!(path = %path.display(), "state saved");
        Ok(())
    }

    /// Get or create a client entry. Returns mutable reference.
    pub fn get_or_create_client(&mut self, id: &str, host_name: &str, mac: &str) -> &mut Client {
        if !self.clients.contains_key(id) {
            tracing::debug!(client_id = id, host_name, mac, "client created");
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
        self.clients.get_mut(id).expect("just inserted")
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
            id: generate_id(),
            name: String::new(),
            stream_id: default_stream.to_string(),
            muted: false,
            clients: vec![client_id.to_string()],
        };
        self.groups.push(group);
        self.groups.last_mut().expect("just pushed")
    }

    /// Remove a client from all groups.
    pub fn remove_client_from_groups(&mut self, client_id: &str) {
        for group in &mut self.groups {
            group.clients.retain(|c| c != client_id);
        }
        // Remove empty groups
        self.groups.retain(|g| !g.clients.is_empty());
    }

    /// Set the clients of a group (C++ Group.SetClients semantics).
    ///
    /// - Clients removed from the target group get their own new group (inheriting the stream).
    /// - Clients added are moved from their old groups (empty old groups are removed).
    /// - If the target group ends up empty, it is removed.
    pub fn set_group_clients(&mut self, group_id: &str, client_ids: &[String]) {
        // Find the target group's stream for inheritance
        let stream_id = self
            .groups
            .iter()
            .find(|g| g.id == group_id)
            .map(|g| g.stream_id.clone())
            .unwrap_or_default();

        // 1. Evict clients NOT in the new list → create new group for each
        if let Some(group) = self.groups.iter_mut().find(|g| g.id == group_id) {
            let evicted: Vec<String> = group
                .clients
                .iter()
                .filter(|c| !client_ids.contains(c))
                .cloned()
                .collect();
            group.clients.retain(|c| client_ids.contains(c));
            for cid in evicted {
                let new_group = Group {
                    id: generate_id(),
                    name: String::new(),
                    stream_id: stream_id.clone(),
                    muted: false,
                    clients: vec![cid],
                };
                self.groups.push(new_group);
            }
        }

        // 2. Add clients to the target group (move from old groups)
        for cid in client_ids {
            let already_in_target = self
                .groups
                .iter()
                .any(|g| g.id == group_id && g.clients.contains(cid));
            if already_in_target {
                continue;
            }
            // Remove from old group
            for group in &mut self.groups {
                group.clients.retain(|c| c != cid);
            }
            // Add to target
            if let Some(group) = self.groups.iter_mut().find(|g| g.id == group_id) {
                group.clients.push(cid.clone());
            }
        }

        // 3. Remove empty groups
        self.groups.retain(|g| !g.clients.is_empty());
    }

    /// Set a group's stream.
    pub fn set_group_stream(&mut self, group_id: &str, stream_id: &str) {
        if let Some(group) = self.groups.iter_mut().find(|g| g.id == group_id) {
            group.stream_id = stream_id.to_string();
        }
    }

    /// Build typed status snapshot.
    pub fn to_status(&self) -> crate::status::ServerStatus {
        use crate::status;
        let groups = self
            .groups
            .iter()
            .map(|g| {
                let clients = g
                    .clients
                    .iter()
                    .filter_map(|cid| self.clients.get(cid))
                    .map(|c| status::Client {
                        id: c.id.clone(),
                        connected: c.connected,
                        config: status::ClientConfig {
                            name: c.config.name.clone(),
                            volume: status::Volume {
                                percent: c.config.volume.percent,
                                muted: c.config.volume.muted,
                            },
                            latency: c.config.latency,
                            ..Default::default()
                        },
                        host: status::Host {
                            name: c.host_name.clone(),
                            mac: c.mac.clone(),
                            ..Default::default()
                        },
                        ..Default::default()
                    })
                    .collect();
                status::Group {
                    id: g.id.clone(),
                    name: g.name.clone(),
                    stream_id: g.stream_id.clone(),
                    muted: g.muted,
                    clients,
                }
            })
            .collect();
        let streams = self
            .streams
            .iter()
            .map(|s| status::Stream {
                id: s.id.clone(),
                status: status::StreamStatus::from(s.status.as_str()),
                uri: status::StreamUri {
                    raw: s.uri.clone(),
                    ..Default::default()
                },
                ..Default::default()
            })
            .collect();
        status::ServerStatus {
            server: status::Server {
                groups,
                streams,
                ..Default::default()
            },
        }
    }
}

fn generate_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    // Seed from time, mix with random-ish bits — matches C++ UUID format
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;
    // Use wrapping arithmetic to spread bits across a 128-bit space
    let a = t.wrapping_mul(6364136223846793005);
    let b = t.wrapping_mul(1442695040888963407).wrapping_add(a);
    format!(
        "{:08x}-{:04x}-{:04x}-{:04x}-{:04x}{:04x}{:04x}",
        (a >> 32) as u32,
        (a >> 16) as u16,
        (a & 0xffff) as u16,
        (b >> 48) as u16,
        (b >> 32) as u16,
        (b >> 16) as u16,
        b as u16,
    )
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
    fn set_group_clients_moves_and_evicts() {
        let mut state = ServerState::default();
        state.get_or_create_client("c1", "h1", "m1");
        state.get_or_create_client("c2", "h2", "m2");
        state.get_or_create_client("c3", "h3", "m3");
        let g1 = state.group_for_client("c1", "s1").id.clone();
        state.group_for_client("c2", "s1");
        state.group_for_client("c3", "s1");
        assert_eq!(state.groups.len(), 3);

        // Move c2 and c3 into g1 (c1's group)
        state.set_group_clients(&g1, &["c1".into(), "c2".into(), "c3".into()]);
        assert_eq!(state.groups.len(), 1);
        assert_eq!(state.groups[0].clients.len(), 3);

        // Evict c2 — should get its own group inheriting the stream
        state.set_group_clients(&g1, &["c1".into(), "c3".into()]);
        assert_eq!(state.groups.len(), 2);
        let evicted_group = state.groups.iter().find(|g| g.id != g1).unwrap();
        assert_eq!(evicted_group.clients, vec!["c2"]);
        assert_eq!(evicted_group.stream_id, "s1");
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
        let status = state.to_status();
        assert_eq!(status.server.groups.len(), 1);
        assert_eq!(status.server.groups[0].clients.len(), 1);
    }

    #[test]
    fn generate_id_is_uuid_format() {
        let id = generate_id();
        // xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx
        let parts: Vec<&str> = id.split('-').collect();
        assert_eq!(parts.len(), 5, "expected 5 UUID parts, got: {id}");
        assert_eq!(parts[0].len(), 8);
        assert_eq!(parts[1].len(), 4);
        assert_eq!(parts[2].len(), 4);
        assert_eq!(parts[3].len(), 4);
        assert_eq!(parts[4].len(), 12);
        // All hex
        assert!(id.replace('-', "").chars().all(|c| c.is_ascii_hexdigit()));
    }
}
