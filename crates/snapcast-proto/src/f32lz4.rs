//! Wire-format constants for the `f32lz4` / `f32lz4e` (encrypted) codec.
//!
//! Single source of truth for the byte-level contract that the server encoder
//! (`snapcast-server`) and the client decoder (`snapcast-client`) must agree
//! on exactly. These values were previously duplicated as literals in both
//! crates (and inside the crypto modules), where they could drift apart with
//! no compile error — a drift would surface only as a silent decode/decrypt
//! failure at runtime. Define them once here; both ends reference them.

/// Codec-header magic identifying an `f32lz4` stream.
pub const F32LZ4_MAGIC: &[u8; 4] = b"F32L";

/// Length of the base `f32lz4` codec header in bytes.
///
/// Layout: `MAGIC(4) + sample_rate: u32(4) + channels: u16(2) + bits: u16(2)`.
pub const F32LZ4_HEADER_LEN: usize = 12;

/// Marker placed after the base header to signal an encrypted stream.
///
/// When present it is immediately followed by [`F32LZ4_SALT_LEN`] salt bytes.
pub const F32LZ4_ENC_MARKER: &[u8; 4] = b"ENC\0";

/// Length of the per-session encryption salt, in bytes.
pub const F32LZ4_SALT_LEN: usize = 16;

/// Total codec-header length when encryption is enabled.
///
/// `base header + ENC marker (4) + salt`.
pub const F32LZ4_ENC_HEADER_LEN: usize =
    F32LZ4_HEADER_LEN + F32LZ4_ENC_MARKER.len() + F32LZ4_SALT_LEN;

// --- ChaCha20-Poly1305 AEAD parameters (key derived via HKDF-SHA256) ---

/// AEAD nonce length for ChaCha20-Poly1305, in bytes.
pub const CRYPTO_NONCE_LEN: usize = 12;

/// AEAD authentication tag length (Poly1305), in bytes.
pub const CRYPTO_TAG_LEN: usize = 16;

/// Derived symmetric key length (ChaCha20 256-bit key), in bytes.
pub const CRYPTO_KEY_LEN: usize = 32;

/// HKDF `info` context string binding the derived key to this codec.
///
/// Both ends must expand with the identical info string or the derived keys
/// differ and every chunk fails to authenticate.
pub const CRYPTO_HKDF_INFO: &[u8] = b"snapcast-f32lz4e";
