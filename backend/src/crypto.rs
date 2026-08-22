//! Credential encryption module.
//!
//! Encrypts mail-account credentials at rest using AES-256-GCM.
//! The encryption key is derived from the user's encrypted DEK
//! (which is itself protected by the master key from `LYRA_MASTER_KEY`).
//!
//! See `docs/specs/2026-08-20-lyra-data-model-spec.md` §3.

use aes_gcm::{Aes256Gcm, KeyInit, Nonce, aead::Aead};
use base64::{Engine, engine::general_purpose::STANDARD as B64};
use hkdf::Hkdf;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::Sha256;

/// Encrypted credential blob stored in the database.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncryptedCredential {
    /// Base64-encoded ciphertext.
    pub ciphertext: String,
    /// Base64-encoded 96-bit nonce.
    pub nonce: String,
}

/// Error type for crypto operations.
#[derive(Debug, thiserror::Error)]
#[allow(dead_code)]
pub enum CryptoError {
    #[error("encryption failed: {0}")]
    Encrypt(String),
    #[error(
        "decryption failed ({0}); data was likely encrypted under a different key — re-add the account or reset the database"
    )]
    Decrypt(String),
    #[error("invalid key length")]
    InvalidKeyLength,
    #[error("invalid nonce length")]
    InvalidNonceLength,
    #[error("base64 decode error: {0}")]
    Base64(#[from] base64::DecodeError),
    #[error("master key not initialized; set LYRA_MASTER_KEY at startup")]
    MasterKeyNotInitialized,
    #[error("no encrypted DEK stored for this user; re-run bootstrap or reset the database")]
    MissingDek,
    #[error("database error while loading the user DEK: {0}")]
    Storage(String),
}

/// Encrypt plaintext using AES-256-GCM with a random nonce.
///
/// # Arguments
/// * `key` - 256-bit (32-byte) encryption key
/// * `plaintext` - data to encrypt
///
/// # Returns
/// `EncryptedCredential` with base64-encoded ciphertext and nonce.
pub fn encrypt(key: &[u8], plaintext: &[u8]) -> Result<EncryptedCredential, CryptoError> {
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|_| CryptoError::InvalidKeyLength)?;

    // Generate random 96-bit nonce
    let mut nonce_arr = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut nonce_arr);

    let nonce = Nonce::from_slice(&nonce_arr);
    let ciphertext = cipher
        .encrypt(nonce, plaintext)
        .map_err(|e| CryptoError::Encrypt(e.to_string()))?;

    Ok(EncryptedCredential {
        ciphertext: B64.encode(&ciphertext),
        nonce: B64.encode(nonce_arr),
    })
}

/// Decrypt an `EncryptedCredential` using AES-256-GCM.
///
/// # Arguments
/// * `key` - 256-bit (32-byte) encryption key (same as used for encryption)
/// * `encrypted` - the encrypted credential blob
///
/// # Returns
/// Decrypted plaintext bytes.
#[allow(dead_code)]
pub fn decrypt(key: &[u8], encrypted: &EncryptedCredential) -> Result<Vec<u8>, CryptoError> {
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|_| CryptoError::InvalidKeyLength)?;

    let nonce_bytes = B64.decode(&encrypted.nonce)?;
    if nonce_bytes.len() != 12 {
        return Err(CryptoError::InvalidNonceLength);
    }
    let nonce = Nonce::from_slice(&nonce_bytes);

    let ciphertext = B64.decode(&encrypted.ciphertext)?;

    cipher
        .decrypt(nonce, ciphertext.as_ref())
        .map_err(|e| CryptoError::Decrypt(e.to_string()))
}

/// Generate a random 256-bit key for testing or initial setup.
#[allow(dead_code)]
pub fn generate_key() -> [u8; 32] {
    let mut key = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut key);
    key
}

/// HKDF-SHA256 info-string prefix binding a KEK to one user.
const KEK_INFO_PREFIX: &str = "lyra-user-kek:v1:";

/// Derive the per-user key-encryption-key (KEK) from the master key.
///
/// HKDF-SHA256 with no salt and an info string that binds the derived key to
/// the user id, so each user gets an independent KEK from the same master key.
pub fn derive_user_kek(master_key: &[u8], user_id: &str) -> [u8; 32] {
    let hk = Hkdf::<Sha256>::new(None, master_key);
    let mut kek = [0u8; 32];
    hk.expand(format!("{KEK_INFO_PREFIX}{user_id}").as_bytes(), &mut kek)
        .expect("32 bytes is a valid HKDF-SHA256 output length");
    kek
}

/// Wrap (encrypt) a data-encryption key with a KEK; returns the JSON blob to
/// store in `lyra_user.encrypted_dek`.
pub fn wrap_dek(kek: &[u8; 32], dek: &[u8; 32]) -> Result<String, CryptoError> {
    let encrypted = encrypt(kek, dek)?;
    serde_json::to_string(&encrypted).map_err(|e| CryptoError::Encrypt(e.to_string()))
}

/// Unwrap a DEK previously stored by [`wrap_dek`].
pub fn unwrap_dek(kek: &[u8; 32], wrapped_json: &str) -> Result<Vec<u8>, CryptoError> {
    let encrypted: EncryptedCredential = serde_json::from_str(wrapped_json).map_err(|e| {
        CryptoError::Decrypt(format!(
            "stored DEK is not a wrapped key blob ({e}); reset the database and re-add accounts"
        ))
    })?;
    decrypt(kek, &encrypted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encrypt_decrypt_roundtrip() {
        let key = generate_key();
        let plaintext = b"my-secret-password";

        let encrypted = encrypt(&key, plaintext).unwrap();
        let decrypted = decrypt(&key, &encrypted).unwrap();

        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn different_nonces() {
        let key = generate_key();
        let plaintext = b"test";

        let enc1 = encrypt(&key, plaintext).unwrap();
        let enc2 = encrypt(&key, plaintext).unwrap();

        // Different nonces should produce different ciphertext
        assert_ne!(enc1.nonce, enc2.nonce);
    }

    #[test]
    fn wrong_key_fails() {
        let key1 = generate_key();
        let key2 = generate_key();
        let plaintext = b"secret";

        let encrypted = encrypt(&key1, plaintext).unwrap();
        assert!(decrypt(&key2, &encrypted).is_err());
    }

    #[test]
    fn kek_differs_per_user() {
        let master = generate_key();
        let kek_a = derive_user_kek(&master, "user-a");
        let kek_b = derive_user_kek(&master, "user-b");
        assert_ne!(kek_a, kek_b);
        // Deterministic for the same user and master key.
        assert_eq!(kek_a, derive_user_kek(&master, "user-a"));
    }

    #[test]
    fn dek_wrap_unwrap_roundtrip() {
        let master = generate_key();
        let kek = derive_user_kek(&master, "user-a");
        let dek = generate_key();

        let wrapped = wrap_dek(&kek, &dek).unwrap();
        assert_eq!(unwrap_dek(&kek, &wrapped).unwrap(), dek);

        // Another user's KEK cannot unwrap it.
        let other_kek = derive_user_kek(&master, "user-b");
        assert!(unwrap_dek(&other_kek, &wrapped).is_err());
    }
}
