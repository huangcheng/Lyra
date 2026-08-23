//! Credential encryption module.
//!
//! Encrypts mail-account credentials at rest using AES-256-GCM.
//! The encryption key is derived from the user's encrypted DEK
//! (which is itself protected by the master key from `LYRA_MASTER_KEY`).
//!
//! See `docs/specs/2026-08-20-lyra-data-model-spec.md` §3.

use aes_gcm::{aead::Aead, Aes256Gcm, KeyInit, Nonce};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
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
    #[error(
        "no encrypted DEK stored for this user; re-add the mail account or reset the database"
    )]
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

/// Generate a random 256-bit key for initial setup (e.g. per-user DEKs).
pub fn generate_key() -> [u8; 32] {
    let mut key = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut key);
    key
}

/// HKDF-SHA256 info-string prefix binding a KEK to one user.
const KEK_INFO_PREFIX: &str = "lyra-user-kek:v1:";

/// Default `LYRA_MASTER_KEY` used by the pre-DEK `get_user_dek` when the env
/// var was unset. First 32 bytes were the AES key for account passwords.
pub(crate) const LEGACY_DEFAULT_MASTER_KEY: &[u8] = b"lyra-default-master-key-for-dev-only";

/// Pad/truncate a master key the way pre-DEK code derived the AES key.
pub(crate) fn pad_master_key_to_dek(master: &[u8]) -> [u8; 32] {
    let mut key = [0u8; 32];
    let n = master.len().min(32);
    key[..n].copy_from_slice(&master[..n]);
    key
}

/// AES keys that may decrypt credentials written before per-user DEKs.
fn legacy_dek_candidates(master_key: &[u8]) -> Vec<[u8; 32]> {
    let current = pad_master_key_to_dek(master_key);
    let default = pad_master_key_to_dek(LEGACY_DEFAULT_MASTER_KEY);
    if current == default {
        vec![current]
    } else {
        vec![current, default]
    }
}

/// Try the pre-DEK padded-master-key scheme. Never logs plaintext.
pub(crate) fn try_decrypt_with_legacy_keys(
    blob: &EncryptedCredential,
    master_key: &[u8],
) -> Option<Vec<u8>> {
    for key in legacy_dek_candidates(master_key) {
        if let Ok(pt) = decrypt(&key, blob) {
            return Some(pt);
        }
    }
    None
}

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

    #[test]
    fn pad_master_key_uses_first_32_bytes() {
        let padded = pad_master_key_to_dek(LEGACY_DEFAULT_MASTER_KEY);
        assert_eq!(&padded, &LEGACY_DEFAULT_MASTER_KEY[..32]);
        let short = pad_master_key_to_dek(b"abc");
        assert_eq!(&short[..3], b"abc");
        assert_eq!(&short[3..], &[0u8; 29]);
    }

    #[test]
    fn legacy_padded_key_decrypts_pre_dek_ciphertext() {
        let master = LEGACY_DEFAULT_MASTER_KEY;
        let key = pad_master_key_to_dek(master);
        let blob = encrypt(&key, b"secret-pass").unwrap();
        assert_eq!(
            try_decrypt_with_legacy_keys(&blob, b"some-other-master-key-32-bytes-ok!!").unwrap(),
            b"secret-pass"
        );
        assert!(
            try_decrypt_with_legacy_keys(&blob, b"unrelated-key-also-32-bytes-long!").is_some()
        );
        // Unrelated + not the default → fail.
        let other = encrypt(
            &pad_master_key_to_dek(b"zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz"),
            b"x",
        )
        .unwrap();
        assert!(
            try_decrypt_with_legacy_keys(&other, b"yyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyy").is_none()
        );
    }
}
