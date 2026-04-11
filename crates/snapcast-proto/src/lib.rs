#![forbid(unsafe_code)]
#![warn(clippy::redundant_closure)]
#![warn(clippy::implicit_clone)]
#![warn(clippy::uninlined_format_args)]
#![warn(missing_docs)]

//! Snapcast binary protocol implementation.
//!
//! This crate implements the Snapcast binary wire protocol, providing
//! serialization and deserialization for all message types exchanged
//! between snapclient and snapserver.
//!
//! # Protocol Overview
//!
//! Every message consists of a [`BaseMessage`] header followed by a typed payload.
//! All multi-byte integers are little-endian.
//!
//! See the [protocol documentation](https://github.com/snapcast/snapcast/blob/master/doc/binary_protocol.md)
//! for the full specification.

pub mod message;
pub mod sample_format;
pub mod status;
pub mod types;

#[cfg(feature = "custom-protocol")]
pub use message::CustomMessage;
pub use message::MessageType;
pub use message::base::{BaseMessage, ProtoError};
pub use sample_format::SampleFormat;
pub use types::Timeval;

/// Default TCP port for binary protocol (streaming clients).
pub const DEFAULT_STREAM_PORT: u16 = 1704;
/// Default TCP port for JSON-RPC control.
pub const DEFAULT_CONTROL_PORT: u16 = 1705;
/// Default HTTP port for JSON-RPC + Snapweb.
pub const DEFAULT_HTTP_PORT: u16 = 1780;
/// Default WebSocket Secure port.
pub const DEFAULT_WSS_PORT: u16 = 1788;
/// Snapcast binary protocol version.
pub const PROTOCOL_VERSION: u32 = 2;
/// Default sample format: 48000 Hz, 16-bit, stereo.
pub const DEFAULT_SAMPLE_FORMAT: SampleFormat = SampleFormat::new(48000, 16, 2);

/// Default pre-shared key for f32lz4e (encrypted f32lz4) codec.
///
/// Used when the `f32lz4e` codec is selected without an explicit PSK.
/// Provides transport obfuscation out of the box — not a substitute for
/// real key management in security-sensitive deployments.
pub const DEFAULT_ENCRYPTION_PSK: &str = "snapcast-f32lz4e-default-psk-v1";
