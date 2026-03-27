//! AES-256-GCM encrypted vault for secrets (API keys, passwords, SSH keys).
//!
//! Secrets are encrypted at rest in PostgreSQL. The master key is loaded from
//! the `MIMIR_VAULT_KEY` environment variable (base64-encoded 32-byte key).
//! Each secret gets a unique 96-bit nonce prepended to the ciphertext.

use aes_gcm::{
    Aes256Gcm, KeyInit, Nonce,
    aead::Aead,
};
use base64::Engine as _;
use rand::RngCore;

/// 32-byte AES-256 key.
pub struct VaultKey([u8; 32]);

impl VaultKey {
    /// Load the vault key from base64-encoded env var `MIMIR_VAULT_KEY`.
    ///
    /// Returns `None` if the env var is missing (vault features disabled).
    /// Returns `Err` if the env var is present but malformed.
    pub fn from_env() -> Result<Option<Self>, String> {
        let encoded = match std::env::var("MIMIR_VAULT_KEY") {
            Ok(v) if !v.is_empty() => v,
            _ => return Ok(None),
        };

        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded.trim())
            .map_err(|e| format!("MIMIR_VAULT_KEY is not valid base64: {e}"))?;

        if bytes.len() != 32 {
            return Err(format!(
                "MIMIR_VAULT_KEY must be exactly 32 bytes, got {}",
                bytes.len()
            ));
        }

        let mut key = [0u8; 32];
        key.copy_from_slice(&bytes);
        Ok(Some(Self(key)))
    }

    /// Encrypt plaintext. Returns nonce (12 bytes) || ciphertext.
    pub fn encrypt(&self, plaintext: &[u8]) -> Result<Vec<u8>, String> {
        let cipher = Aes256Gcm::new_from_slice(&self.0)
            .map_err(|e| format!("cipher init failed: {e}"))?;

        let mut nonce_bytes = [0u8; 12];
        rand::thread_rng().fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);

        let ciphertext = cipher
            .encrypt(nonce, plaintext)
            .map_err(|e| format!("encryption failed: {e}"))?;

        // Prepend nonce to ciphertext for storage
        let mut output = Vec::with_capacity(12 + ciphertext.len());
        output.extend_from_slice(&nonce_bytes);
        output.extend_from_slice(&ciphertext);
        Ok(output)
    }

    /// Decrypt data produced by `encrypt`. Input is nonce (12 bytes) || ciphertext.
    pub fn decrypt(&self, data: &[u8]) -> Result<Vec<u8>, String> {
        if data.len() < 12 {
            return Err("encrypted data too short (need at least 12-byte nonce)".into());
        }

        let cipher = Aes256Gcm::new_from_slice(&self.0)
            .map_err(|e| format!("cipher init failed: {e}"))?;

        let nonce = Nonce::from_slice(&data[..12]);
        let ciphertext = &data[12..];

        cipher
            .decrypt(nonce, ciphertext)
            .map_err(|e| format!("decryption failed (wrong key or corrupted data): {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_key() -> VaultKey {
        VaultKey([0x42u8; 32])
    }

    #[test]
    fn round_trip() {
        let key = test_key();
        let plaintext = b"SUPER_SECRET_API_KEY_12345";

        let encrypted = key.encrypt(plaintext).unwrap();
        // Encrypted data should be larger (12 nonce + 16 tag + plaintext)
        assert!(encrypted.len() > plaintext.len());

        let decrypted = key.decrypt(&encrypted).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn different_nonces_per_encrypt() {
        let key = test_key();
        let plaintext = b"same input";

        let a = key.encrypt(plaintext).unwrap();
        let b = key.encrypt(plaintext).unwrap();

        // Nonces should differ (first 12 bytes)
        assert_ne!(&a[..12], &b[..12]);
        // Both should decrypt to the same plaintext
        assert_eq!(key.decrypt(&a).unwrap(), plaintext);
        assert_eq!(key.decrypt(&b).unwrap(), plaintext);
    }

    #[test]
    fn wrong_key_fails() {
        let key1 = VaultKey([0x42u8; 32]);
        let key2 = VaultKey([0x99u8; 32]);
        let plaintext = b"secret";

        let encrypted = key1.encrypt(plaintext).unwrap();
        assert!(key2.decrypt(&encrypted).is_err());
    }

    #[test]
    fn corrupted_data_fails() {
        let key = test_key();
        let plaintext = b"secret";

        let mut encrypted = key.encrypt(plaintext).unwrap();
        // Flip a byte in the ciphertext
        let last = encrypted.len() - 1;
        encrypted[last] ^= 0xFF;

        assert!(key.decrypt(&encrypted).is_err());
    }

    #[test]
    fn too_short_fails() {
        let key = test_key();
        assert!(key.decrypt(&[0u8; 5]).is_err());
    }
}
