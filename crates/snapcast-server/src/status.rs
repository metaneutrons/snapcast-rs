//! Public read-only status types returned by [`GetStatus`](crate::ServerCommand::GetStatus).
//!
//! These are the canonical types for server state queries. They derive
//! `Serialize` so consumers can convert to JSON (or any other format)
//! at their layer.

use std::collections::HashMap;

use serde::Serialize;

/// Full server status snapshot.
#[derive(Debug, Clone, Serialize)]
pub struct ServerStatus {
    /// All groups (each containing its clients).
    pub groups: Vec<GroupStatus>,
    /// All configured streams.
    pub streams: Vec<StreamStatus>,
}

/// A group of clients sharing the same stream.
#[derive(Debug, Clone, Serialize)]
pub struct GroupStatus {
    /// Unique group ID.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Stream ID this group is playing.
    pub stream_id: String,
    /// Group mute state.
    pub muted: bool,
    /// Clients in this group.
    pub clients: Vec<ClientStatus>,
}

/// A connected or previously-seen client.
#[derive(Debug, Clone, Serialize)]
pub struct ClientStatus {
    /// Unique client ID.
    pub id: String,
    /// Whether currently connected.
    pub connected: bool,
    /// Client configuration.
    pub config: ClientConfig,
    /// Host information.
    pub host: HostInfo,
}

/// Client configuration (persisted across restarts).
#[derive(Debug, Clone, Serialize)]
pub struct ClientConfig {
    /// Display name.
    pub name: String,
    /// Volume settings.
    pub volume: VolumeInfo,
    /// Additional latency in milliseconds.
    pub latency: i32,
}

/// Volume state.
#[derive(Debug, Clone, Serialize)]
pub struct VolumeInfo {
    /// Volume percentage (0–100).
    pub percent: u16,
    /// Mute state.
    pub muted: bool,
}

/// Host identification.
#[derive(Debug, Clone, Serialize)]
pub struct HostInfo {
    /// Hostname.
    pub name: String,
    /// MAC address.
    pub mac: String,
}

/// An audio stream source.
#[derive(Debug, Clone, Serialize)]
pub struct StreamStatus {
    /// Stream ID.
    pub id: String,
    /// Status: "playing", "idle", "unknown".
    pub status: String,
    /// Source URI.
    pub uri: String,
    /// Stream properties (metadata).
    pub properties: HashMap<String, serde_json::Value>,
}
