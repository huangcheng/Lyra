//! TOTP enrollment helpers.

use totp_rs::{Algorithm, TOTP};

use crate::crypto::{self, CryptoError};

use super::types::AuthError;

pub(crate) fn encrypt_totp_secret(dek: &[u8], secret_base32: &str) -> Result<String, CryptoError> {
    let encrypted = crypto::encrypt(dek, secret_base32.as_bytes())?;
    serde_json::to_string(&encrypted).map_err(|e| CryptoError::Encrypt(e.to_string()))
}

/// Decrypt a stored TOTP secret blob back to its base32 form.
pub(crate) fn decrypt_totp_secret(dek: &[u8], stored: &str) -> Result<String, CryptoError> {
    let encrypted: crypto::EncryptedCredential = serde_json::from_str(stored).map_err(|e| {
        CryptoError::Decrypt(format!(
            "stored TOTP secret is not an encrypted blob ({e}); disable 2FA and re-enroll, or reset the database"
        ))
    })?;
    let bytes = crypto::decrypt(dek, &encrypted)?;
    String::from_utf8(bytes)
        .map_err(|e| CryptoError::Decrypt(format!("TOTP secret is not valid UTF-8: {e}")))
}

pub(crate) fn build_totp(secret_base32: &str, username: &str) -> Result<TOTP, AuthError> {
    let secret_bytes = base32::decode(base32::Alphabet::Rfc4648 { padding: false }, secret_base32)
        .ok_or_else(|| {
            tracing::error!("Invalid TOTP secret encoding");
            AuthError::internal("Failed to build TOTP")
        })?;
    build_totp_from_raw(&secret_bytes, username)
}

pub(crate) fn build_totp_from_raw(secret_bytes: &[u8], username: &str) -> Result<TOTP, AuthError> {
    TOTP::new(
        Algorithm::SHA1,
        6,
        1,
        30,
        secret_bytes.to_vec(),
        Some("Lyra".to_string()),
        username.to_string(),
    )
    .map_err(|e| {
        tracing::error!("Failed to create TOTP: {e}");
        AuthError::internal("Failed to build TOTP")
    })
}

/// Find the timestep at which `code` is valid (current ± 1 step, matching the
/// skew of `TOTP::check_current`) so replay protection can compare steps.
/// Returns `None` when the code matches no step in the window.
pub(crate) fn matched_totp_step(totp: &TOTP, code: &str) -> Option<u64> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    let step = totp.step;
    let current = now / step;
    (current.saturating_sub(1)..=current + 1).find(|&s| totp.generate(s * step) == code)
}

// ── Tests ───────────────────────────────────────────────────────────
