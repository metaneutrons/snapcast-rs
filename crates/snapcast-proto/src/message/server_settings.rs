//! Server Settings message (type=3).
//!
//! Sent from server to client. Contains buffer size, latency, volume, and mute state.

use std::io::{Read, Write};

use serde::{Deserialize, Serialize};

use crate::message::base::ProtoError;
use crate::message::wire;

/// Server settings JSON payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerSettings {
    pub buffer_ms: i32,
    pub latency: i32,
    pub volume: u16,
    pub muted: bool,
}

impl ServerSettings {
    pub fn wire_size(&self) -> u32 {
        let json = serde_json::to_string(self).unwrap_or_default();
        wire::string_wire_size(&json)
    }

    pub fn read_from<R: Read>(r: &mut R) -> Result<Self, ProtoError> {
        let json_str = wire::read_string(r)?;
        serde_json::from_str(&json_str)
            .map_err(|e| ProtoError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, e)))
    }

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
        let original = ServerSettings {
            buffer_ms: 1000,
            latency: 0,
            volume: 100,
            muted: false,
        };
        let mut buf = Vec::new();
        original.write_to(&mut buf).unwrap();
        let mut cursor = std::io::Cursor::new(&buf);
        let decoded = ServerSettings::read_from(&mut cursor).unwrap();
        assert_eq!(original, decoded);
    }

    #[test]
    fn deserialize_cpp_json() {
        let json = r#"{"bufferMs":1000,"latency":0,"muted":false,"volume":100}"#;
        let ss: ServerSettings = serde_json::from_str(json).unwrap();
        assert_eq!(ss.buffer_ms, 1000);
        assert_eq!(ss.volume, 100);
        assert!(!ss.muted);
    }

    #[test]
    fn json_field_names_match_cpp() {
        let ss = ServerSettings {
            buffer_ms: 500,
            latency: 10,
            volume: 80,
            muted: true,
        };
        let json_str = serde_json::to_string(&ss).unwrap();
        assert!(json_str.contains("\"bufferMs\""));
        assert!(json_str.contains("\"latency\""));
        assert!(json_str.contains("\"volume\""));
        assert!(json_str.contains("\"muted\""));
    }
}
