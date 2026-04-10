//! ChaCha20-Poly1305 decryption for audio chunks.

use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use hkdf::Hkdf;
use sha2::Sha256;

const NONCE_SIZE: usize = 12;

fn derive_key(psk: &[u8], salt: &[u8]) -> [u8; 32] {
    let hk = Hkdf::<Sha256>::new(Some(salt), psk);
    let mut key = [0u8; 32];
    hk.expand(b"snapcast-f32lz4e", &mut key)
        .expect("32 bytes is a valid HKDF-SHA256 output length");
    key
}

/// Audio chunk decryptor.
pub struct ChunkDecryptor {
    cipher: ChaCha20Poly1305,
}

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
    fn wrong_key_fails() {
        // Encrypt with server's crypto module would be needed for a full test.
        // Here we just verify that decryption of garbage fails gracefully.
        let dec = ChunkDecryptor::new("key", b"salt");
        assert!(dec.decrypt(&[0u8; 64]).is_err());
    }
}
