//! ChaCha20-Poly1305 encryption for audio chunks.
//!
//! Derives a 256-bit key from a pre-shared key via HKDF-SHA256.
//! Each chunk is encrypted with a unique nonce (counter-based).

use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use hkdf::Hkdf;
use sha2::Sha256;
use snapcast_proto::f32lz4::{CRYPTO_HKDF_INFO, CRYPTO_KEY_LEN, CRYPTO_NONCE_LEN};
// CRYPTO_TAG_LEN is only referenced from the roundtrip tests (the server path
// encrypts and never inspects the tag length).
#[cfg(test)]
use snapcast_proto::f32lz4::CRYPTO_TAG_LEN;

/// Derives a 256-bit encryption key from a PSK and salt via HKDF-SHA256.
fn derive_key(psk: &[u8], salt: &[u8]) -> [u8; CRYPTO_KEY_LEN] {
    let hk = Hkdf::<Sha256>::new(Some(salt), psk);
    let mut key = [0u8; CRYPTO_KEY_LEN];
    hk.expand(CRYPTO_HKDF_INFO, &mut key)
        .expect("CRYPTO_KEY_LEN is a valid HKDF-SHA256 output length");
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

    /// Encrypt a chunk. Returns `[nonce][ciphertext + tag]`.
    pub fn encrypt(&mut self, plaintext: &[u8]) -> Result<Vec<u8>, chacha20poly1305::Error> {
        let mut nonce_bytes = [0u8; CRYPTO_NONCE_LEN];
        nonce_bytes[..8].copy_from_slice(&self.counter.to_le_bytes());
        self.counter += 1;

        let nonce = Nonce::from(nonce_bytes);
        let ciphertext = self.cipher.encrypt(&nonce, plaintext)?;

        let mut out = Vec::with_capacity(CRYPTO_NONCE_LEN + ciphertext.len());
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

    /// Decrypt a chunk. Input: `[nonce][ciphertext + tag]`.
    pub fn decrypt(&self, data: &[u8]) -> Result<Vec<u8>, chacha20poly1305::Error> {
        if data.len() < CRYPTO_NONCE_LEN + CRYPTO_TAG_LEN {
            return Err(chacha20poly1305::Error);
        }
        let nonce = Nonce::from_slice(&data[..CRYPTO_NONCE_LEN]);
        self.cipher.decrypt(nonce, &data[CRYPTO_NONCE_LEN..])
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

        // nonce + plaintext + tag
        assert_eq!(
            encrypted.len(),
            CRYPTO_NONCE_LEN + plaintext.len() + CRYPTO_TAG_LEN
        );

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

        // Nonces should differ (first CRYPTO_NONCE_LEN bytes)
        assert_ne!(&a[..CRYPTO_NONCE_LEN], &b[..CRYPTO_NONCE_LEN]);
    }
}
