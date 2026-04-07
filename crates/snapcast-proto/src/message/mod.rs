//! Protocol message types.

pub mod base;
pub mod client_info;
pub mod codec_header;
pub mod error;
pub mod factory;
pub mod hello;
pub mod server_settings;
pub mod time;
pub mod wire;
pub mod wire_chunk;

/// Message type identifiers matching the C++ `message_type` enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum MessageType {
    Base = 0,
    CodecHeader = 1,
    WireChunk = 2,
    ServerSettings = 3,
    Time = 4,
    Hello = 5,
    // 6 is unused (was StreamTags)
    ClientInfo = 7,
    Error = 8,
}

impl MessageType {
    /// Parse from a raw `u16` value.
    pub fn from_u16(value: u16) -> Option<Self> {
        match value {
            0 => Some(Self::Base),
            1 => Some(Self::CodecHeader),
            2 => Some(Self::WireChunk),
            3 => Some(Self::ServerSettings),
            4 => Some(Self::Time),
            5 => Some(Self::Hello),
            7 => Some(Self::ClientInfo),
            8 => Some(Self::Error),
            _ => None,
        }
    }
}

impl From<MessageType> for u16 {
    fn from(mt: MessageType) -> Self {
        mt as u16
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_all_message_types() {
        let types = [
            MessageType::Base,
            MessageType::CodecHeader,
            MessageType::WireChunk,
            MessageType::ServerSettings,
            MessageType::Time,
            MessageType::Hello,
            MessageType::ClientInfo,
            MessageType::Error,
        ];
        for mt in types {
            let raw: u16 = mt.into();
            assert_eq!(MessageType::from_u16(raw), Some(mt));
        }
    }

    #[test]
    fn unknown_message_type_returns_none() {
        assert_eq!(MessageType::from_u16(6), None);
        assert_eq!(MessageType::from_u16(9), None);
        assert_eq!(MessageType::from_u16(u16::MAX), None);
    }
}
