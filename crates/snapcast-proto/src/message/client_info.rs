//! Client Info message (type=7).
//!
//! Sent from client to server to report volume and mute state changes.

use std::io::{Read, Write};

use serde::{Deserialize, Serialize};

use crate::message::base::ProtoError;
use crate::message::wire;

/// Client info JSON payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClientInfo {
    /// Playback volume (0–100).
    pub volume: u16,
    /// Whether the client is muted.
    pub muted: bool,
}

impl ClientInfo {
    /// Wire size of the JSON payload including length prefix.
    pub fn wire_size(&self) -> u32 {
        let json = serde_json::to_string(self).unwrap_or_default();
        wire::string_wire_size(&json)
    }

    /// Deserialize client info from a reader.
    pub fn read_from<R: Read>(r: &mut R) -> Result<Self, ProtoError> {
        let json_str = wire::read_string(r)?;
        serde_json::from_str(&json_str)
            .map_err(|e| ProtoError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, e)))
    }

    /// Serialize client info to a writer.
    pub fn write_to<W: Write>(&self, w: &mut W) -> Result<(), ProtoError> {
        let json_str = serde_json::to_string(self)
            .map_err(|e| ProtoError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, e)))?;
        wire::write_string(w, &json_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let original = ClientInfo {
            volume: 75,
            muted: false,
        };
        let mut buf = Vec::new();
        original.write_to(&mut buf).unwrap();
        let mut cursor = std::io::Cursor::new(&buf);
        let decoded = ClientInfo::read_from(&mut cursor).unwrap();
        assert_eq!(original, decoded);
    }

    #[test]
    fn deserialize_cpp_json() {
        let json = r#"{"volume":100,"muted":false}"#;
        let ci: ClientInfo = serde_json::from_str(json).unwrap();
        assert_eq!(ci.volume, 100);
        assert!(!ci.muted);
    }
}
