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
pub mod types;

pub use message::MessageType;
pub use message::base::{BaseMessage, ProtoError};
pub use sample_format::SampleFormat;
pub use types::Timeval;
