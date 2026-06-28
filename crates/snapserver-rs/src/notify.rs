//! JSON-RPC notification builders — single source of truth for the
//! `(method, params)` shape of each control notification.
//!
//! Each notification is emitted from two independent places: as the echo of a
//! successful `Set*` command (see `jsonrpc.rs`) and as the fan-out of an
//! internal `ServerEvent` (see `main.rs`). Authoring the shape once here keeps
//! those two paths byte-compatible — they were previously hand-duplicated,
//! which is exactly how a field-name slip (e.g. `mute` vs `muted`) stays live.

use serde_json::{Value, json};

fn notification(method: &str, params: Value) -> Value {
    json!({"jsonrpc": "2.0", "method": method, "params": params})
}

/// `Client.OnVolumeChanged` — a client's volume/mute changed.
pub(crate) fn client_on_volume_changed(client_id: &str, percent: u16, muted: bool) -> Value {
    notification(
        "Client.OnVolumeChanged",
        json!({"id": client_id, "volume": {"percent": percent, "muted": muted}}),
    )
}

/// `Client.OnLatencyChanged` — a client's latency changed.
pub(crate) fn client_on_latency_changed(client_id: &str, latency: i32) -> Value {
    notification(
        "Client.OnLatencyChanged",
        json!({"id": client_id, "latency": latency}),
    )
}

/// `Client.OnNameChanged` — a client was renamed.
pub(crate) fn client_on_name_changed(client_id: &str, name: &str) -> Value {
    notification(
        "Client.OnNameChanged",
        json!({"id": client_id, "name": name}),
    )
}

/// `Group.OnMute` — a group's mute state changed.
pub(crate) fn group_on_mute(group_id: &str, muted: bool) -> Value {
    notification("Group.OnMute", json!({"id": group_id, "mute": muted}))
}

/// `Group.OnStreamChanged` — a group's stream assignment changed.
pub(crate) fn group_on_stream_changed(group_id: &str, stream_id: &str) -> Value {
    notification(
        "Group.OnStreamChanged",
        json!({"id": group_id, "stream_id": stream_id}),
    )
}

/// `Group.OnNameChanged` — a group was renamed.
pub(crate) fn group_on_name_changed(group_id: &str, name: &str) -> Value {
    notification("Group.OnNameChanged", json!({"id": group_id, "name": name}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn volume_uses_muted_not_mute() {
        let n = client_on_volume_changed("c1", 80, true);
        assert_eq!(n["method"], "Client.OnVolumeChanged");
        assert_eq!(n["params"]["volume"]["percent"], 80);
        assert_eq!(n["params"]["volume"]["muted"], true);
    }

    #[test]
    fn group_mute_uses_mute_key() {
        // Group.OnMute deliberately uses "mute" (not "muted") — the trap this
        // module exists to prevent from drifting between the two emit paths.
        let n = group_on_mute("g1", true);
        assert_eq!(n["params"]["mute"], true);
        assert!(n["params"]["muted"].is_null());
    }
}
