//! Base message header — the first 26 bytes of every Snapcast protocol message.

use std::io::{self, Read, Write};

use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use thiserror::Error;

use super::MessageType;
use crate::types::Timeval;

/// Errors that can occur during message parsing.
#[derive(Debug, Error)]
pub enum ProtoError {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("unknown message type: {0}")]
    UnknownMessageType(u16),
}

/// Base message header (26 bytes).
///
/// Every Snapcast protocol message starts with this header, followed by
/// `size` bytes of typed payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaseMessage {
    pub msg_type: MessageType,
    pub id: u16,
    pub refers_to: u16,
    pub sent: Timeval,
    pub received: Timeval,
    pub size: u32,
}

impl BaseMessage {
    /// Size of the serialized header in bytes.
    pub const HEADER_SIZE: usize = 26;

    /// Deserialize a base message header from a reader.
    pub fn read_from<R: Read>(r: &mut R) -> Result<Self, ProtoError> {
        let raw_type = r.read_u16::<LittleEndian>()?;
        let msg_type =
            MessageType::from_u16(raw_type).ok_or(ProtoError::UnknownMessageType(raw_type))?;
        let id = r.read_u16::<LittleEndian>()?;
        let refers_to = r.read_u16::<LittleEndian>()?;
        let sent = Timeval::read_from(r)?;
        let received = Timeval::read_from(r)?;
        let size = r.read_u32::<LittleEndian>()?;
        Ok(Self {
            msg_type,
            id,
            refers_to,
            sent,
            received,
            size,
        })
    }

    /// Serialize a base message header to a writer.
    pub fn write_to<W: Write>(&self, w: &mut W) -> Result<(), ProtoError> {
        w.write_u16::<LittleEndian>(self.msg_type.into())?;
        w.write_u16::<LittleEndian>(self.id)?;
        w.write_u16::<LittleEndian>(self.refers_to)?;
        self.sent.write_to(w)?;
        self.received.write_to(w)?;
        w.write_u32::<LittleEndian>(self.size)?;
        Ok(())
    }

    /// Serialize to a new `Vec<u8>`.
    pub fn to_bytes(&self) -> Result<Vec<u8>, ProtoError> {
        let mut buf = Vec::with_capacity(Self::HEADER_SIZE);
        self.write_to(&mut buf)?;
        Ok(buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test vector: a Hello message header as the C++ code would serialize it.
    ///
    /// Fields (all little-endian):
    ///   type=5 (Hello), id=1, refersTo=0,
    ///   sent.sec=1000, sent.usec=500000,
    ///   received.sec=0, received.usec=0,
    ///   size=42
    const HELLO_HEADER_BYTES: [u8; 26] = [
        0x05, 0x00, // type = 5 (Hello)
        0x01, 0x00, // id = 1
        0x00, 0x00, // refersTo = 0
        0xE8, 0x03, 0x00, 0x00, // sent.sec = 1000
        0x20, 0xA1, 0x07, 0x00, // sent.usec = 500000
        0x00, 0x00, 0x00, 0x00, // received.sec = 0
        0x00, 0x00, 0x00, 0x00, // received.usec = 0
        0x2A, 0x00, 0x00, 0x00, // size = 42
    ];

    fn hello_header() -> BaseMessage {
        BaseMessage {
            msg_type: MessageType::Hello,
            id: 1,
            refers_to: 0,
            sent: Timeval {
                sec: 1000,
                usec: 500_000,
            },
            received: Timeval { sec: 0, usec: 0 },
            size: 42,
        }
    }

    #[test]
    fn serialize_hello_header() {
        let msg = hello_header();
        let bytes = msg.to_bytes().unwrap();
        assert_eq!(bytes.as_slice(), &HELLO_HEADER_BYTES);
    }

    #[test]
    fn deserialize_hello_header() {
        let mut cursor = io::Cursor::new(&HELLO_HEADER_BYTES);
        let msg = BaseMessage::read_from(&mut cursor).unwrap();
        assert_eq!(msg, hello_header());
    }

    #[test]
    fn round_trip() {
        let original = hello_header();
        let bytes = original.to_bytes().unwrap();
        let mut cursor = io::Cursor::new(&bytes);
        let decoded = BaseMessage::read_from(&mut cursor).unwrap();
        assert_eq!(original, decoded);
    }

    #[test]
    fn header_size_is_26_bytes() {
        let msg = hello_header();
        let bytes = msg.to_bytes().unwrap();
        assert_eq!(bytes.len(), BaseMessage::HEADER_SIZE);
    }

    #[test]
    fn unknown_type_returns_error() {
        let mut bad_bytes = HELLO_HEADER_BYTES;
        bad_bytes[0] = 0xFF;
        bad_bytes[1] = 0xFF;
        let mut cursor = io::Cursor::new(&bad_bytes);
        let err = BaseMessage::read_from(&mut cursor).unwrap_err();
        assert!(matches!(err, ProtoError::UnknownMessageType(0xFFFF)));
    }
}
