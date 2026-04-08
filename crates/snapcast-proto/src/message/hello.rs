//! Hello message (type=5).
//!
//! Sent by the client when connecting to the server. Contains client
//! identification and optional authentication info as a JSON payload.

use std::io::{Read, Write};

use serde::{Deserialize, Serialize};

use crate::message::base::ProtoError;
use crate::message::wire;

/// Authentication info (optional).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Auth {
    /// Authentication scheme (e.g. "Basic").
    pub scheme: String,
    /// Scheme-specific parameter (e.g. base64 credentials).
    pub param: String,
}

/// Hello message JSON payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Hello {
    /// Client MAC address.
    #[serde(rename = "MAC")]
    pub mac: String,
    /// Client hostname.
    pub host_name: String,
    /// Client software version.
    pub version: String,
    /// Client application name.
    pub client_name: String,
    /// Client operating system.
    #[serde(rename = "OS")]
    pub os: String,
    /// Client CPU architecture.
    pub arch: String,
    /// Client instance number.
    pub instance: u32,
    /// Unique client identifier.
    #[serde(rename = "ID")]
    pub id: String,
    /// Protocol version supported by the client.
    pub snap_stream_protocol_version: u32,
    /// Optional authentication info.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth: Option<Auth>,
}

impl Hello {
    /// Wire size: u32 length prefix + JSON bytes.
    pub fn wire_size(&self) -> u32 {
        let json = serde_json::to_string(self).unwrap_or_default();
        wire::string_wire_size(&json)
    }

    /// Deserialize a Hello message from a reader.
    pub fn read_from<R: Read>(r: &mut R) -> Result<Self, ProtoError> {
        let json_str = wire::read_string(r)?;
        serde_json::from_str(&json_str)
            .map_err(|e| ProtoError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, e)))
    }

    /// Serialize a Hello message to a writer.
    pub fn write_to<W: Write>(&self, w: &mut W) -> Result<(), ProtoError> {
        let json_str = serde_json::to_string(self)
            .map_err(|e| ProtoError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, e)))?;
        wire::write_string(w, &json_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_hello() -> Hello {
        Hello {
            mac: "00:11:22:33:44:55".into(),
            host_name: "testhost".into(),
            version: "0.32.0".into(),
            client_name: "Snapclient".into(),
            os: "Linux".into(),
            arch: "x86_64".into(),
            instance: 1,
            id: "00:11:22:33:44:55".into(),
            snap_stream_protocol_version: 2,
            auth: None,
        }
    }

    #[test]
    fn round_trip() {
        let original = sample_hello();
        let mut buf = Vec::new();
        original.write_to(&mut buf).unwrap();
        let mut cursor = std::io::Cursor::new(&buf);
        let decoded = Hello::read_from(&mut cursor).unwrap();
        assert_eq!(original, decoded);
    }

    #[test]
    fn round_trip_with_auth() {
        let mut hello = sample_hello();
        hello.auth = Some(Auth {
            scheme: "Basic".into(),
            param: "dXNlcjpwYXNz".into(),
        });
        let mut buf = Vec::new();
        hello.write_to(&mut buf).unwrap();
        let mut cursor = std::io::Cursor::new(&buf);
        let decoded = Hello::read_from(&mut cursor).unwrap();
        assert_eq!(hello, decoded);
        assert_eq!(decoded.auth.unwrap().scheme, "Basic");
    }

    #[test]
    fn json_field_names_match_cpp() {
        let hello = sample_hello();
        let json_str = serde_json::to_string(&hello).unwrap();
        // Verify C++ field names are used
        assert!(json_str.contains("\"MAC\""));
        assert!(json_str.contains("\"HostName\""));
        assert!(json_str.contains("\"SnapStreamProtocolVersion\""));
        assert!(json_str.contains("\"ID\""));
        assert!(json_str.contains("\"OS\""));
        // Auth should be absent when None
        assert!(!json_str.contains("\"Auth\""));
    }

    #[test]
    fn deserialize_cpp_json() {
        // JSON as the C++ server would produce
        let json = r#"{"Arch":"x86_64","ClientName":"Snapclient","HostName":"myhost","ID":"aa:bb:cc:dd:ee:ff","Instance":1,"MAC":"aa:bb:cc:dd:ee:ff","OS":"Arch Linux","SnapStreamProtocolVersion":2,"Version":"0.32.0"}"#;
        let hello: Hello = serde_json::from_str(json).unwrap();
        assert_eq!(hello.mac, "aa:bb:cc:dd:ee:ff");
        assert_eq!(hello.snap_stream_protocol_version, 2);
        assert!(hello.auth.is_none());
    }
}
