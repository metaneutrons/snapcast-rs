//! ChaCha20-Poly1305 encryption for audio chunks.
//!
//! Derives a 256-bit key from a pre-shared key via HKDF-SHA256.
//! Each chunk is encrypted with a unique nonce (counter-based).

use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use hkdf::Hkdf;
use sha2::Sha256;

/// Nonce size (12 bytes).
const NONCE_SIZE: usize = 12;

/// Derives a 256-bit encryption key from a PSK and salt via HKDF-SHA256.
fn derive_key(psk: &[u8], salt: &[u8]) -> [u8; 32] {
    let hk = Hkdf::<Sha256>::new(Some(salt), psk);
    let mut key = [0u8; 32];
    hk.expand(b"snapcast-f32lz4e", &mut key)
        .expect("32 bytes is a valid HKDF-SHA256 output length");
    key
}

/// Audio chunk encryptor.
pub struct ChunkEncryptor {
    cipher: ChaCha20Poly1305,
    counter: u64,
}

impl ChunkEncryptor {
    /// Create from PSK and session salt.
    pub fn new(psk: &str, salt: &[u8]) -> Self {
        let key = derive_key(psk.as_bytes(), salt);
        Self {
            cipher: ChaCha20Poly1305::new(&key.into()),
            counter: 0,
        }
    }

    /// Encrypt a chunk. Returns `[12-byte nonce][ciphertext + 16-byte tag]`.
    pub fn encrypt(&mut self, plaintext: &[u8]) -> Result<Vec<u8>, chacha20poly1305::Error> {
        let mut nonce_bytes = [0u8; NONCE_SIZE];
        nonce_bytes[..8].copy_from_slice(&self.counter.to_le_bytes());
        self.counter += 1;

        let nonce = Nonce::from(nonce_bytes);
        let ciphertext = self.cipher.encrypt(&nonce, plaintext)?;

        let mut out = Vec::with_capacity(NONCE_SIZE + ciphertext.len());
        out.extend_from_slice(&nonce_bytes);
        out.extend_from_slice(&ciphertext);
        Ok(out)
    }
}

/// Audio chunk decryptor — see `snapcast-client` crate for the client-side implementation.
/// Kept here only for roundtrip tests.
#[cfg(test)]
pub struct ChunkDecryptor {
    cipher: ChaCha20Poly1305,
}

#[cfg(test)]
impl ChunkDecryptor {
    /// Create from PSK and session salt.
    pub fn new(psk: &str, salt: &[u8]) -> Self {
        let key = derive_key(psk.as_bytes(), salt);
        Self {
            cipher: ChaCha20Poly1305::new(&key.into()),
        }
    }

    /// Decrypt a chunk. Input: `[12-byte nonce][ciphertext + 16-byte tag]`.
    pub fn decrypt(&self, data: &[u8]) -> Result<Vec<u8>, chacha20poly1305::Error> {
        if data.len() < NONCE_SIZE + 16 {
            return Err(chacha20poly1305::Error);
        }
        let nonce = Nonce::from_slice(&data[..NONCE_SIZE]);
        self.cipher.decrypt(nonce, &data[NONCE_SIZE..])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encrypt_decrypt_roundtrip() {
        let salt = b"test-session-salt";
        let mut enc = ChunkEncryptor::new("my-secret", salt);
        let dec = ChunkDecryptor::new("my-secret", salt);

        let plaintext = b"hello audio data";
        let encrypted = enc.encrypt(plaintext).unwrap();

        // 12 nonce + 16 plaintext + 16 tag = 44
        assert_eq!(encrypted.len(), NONCE_SIZE + plaintext.len() + 16);

        let decrypted = dec.decrypt(&encrypted).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn wrong_key_fails() {
        let salt = b"test-salt";
        let mut enc = ChunkEncryptor::new("correct-key", salt);
        let dec = ChunkDecryptor::new("wrong-key", salt);

        let encrypted = enc.encrypt(b"secret audio").unwrap();
        assert!(dec.decrypt(&encrypted).is_err());
    }

    #[test]
    fn nonce_increments() {
        let salt = b"nonce-test";
        let mut enc = ChunkEncryptor::new("key", salt);

        let a = enc.encrypt(b"chunk1").unwrap();
        let b = enc.encrypt(b"chunk2").unwrap();

        // Nonces should differ (first 12 bytes)
        assert_ne!(&a[..NONCE_SIZE], &b[..NONCE_SIZE]);
    }
}
