//! Password hashing and validation.

use argon2::{
    Argon2, PasswordHash, PasswordHasher, PasswordVerifier,
    password_hash::{SaltString, rand_core::OsRng},
};
use axum::http::StatusCode;
use zeroize::Zeroizing;

use super::MAX_PASSWORD_LENGTH;
use super::types::AuthError;

pub(crate) async fn hash_password(password: &str) -> Result<String, AuthError> {
    let password = Zeroizing::new(password.to_owned());
    tokio::task::spawn_blocking(move || {
        let salt = SaltString::generate(&mut OsRng);
        let argon2 = Argon2::default();
        argon2
            .hash_password(password.as_bytes(), &salt)
            .map_err(|e| {
                tracing::error!("Password hashing failed: {e}");
                AuthError::internal("Failed to hash password")
            })
            .map(|h| h.to_string())
    })
    .await
    .map_err(|e| {
        tracing::error!("Password hash task failed: {e}");
        AuthError::internal("Failed to hash password")
    })?
}

pub(crate) async fn verify_password(password: &str, hash: &str) -> Result<bool, StatusCode> {
    let password = Zeroizing::new(password.to_owned());
    let hash = hash.to_owned();
    tokio::task::spawn_blocking(move || {
        let parsed_hash = PasswordHash::new(&hash).map_err(|e| {
            tracing::error!("Invalid password hash format: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
        Ok(Argon2::default()
            .verify_password(password.as_bytes(), &parsed_hash)
            .is_ok())
    })
    .await
    .map_err(|e| {
        tracing::error!("Password verify task failed: {e}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?
}

pub(crate) fn validate_password(password: &str, min_length: usize) -> Result<(), String> {
    if password.len() > MAX_PASSWORD_LENGTH {
        return Err(format!(
            "Password must be at most {MAX_PASSWORD_LENGTH} characters"
        ));
    }
    let chars = password.chars().count();
    if chars < min_length {
        return Err(format!("Password must be at least {min_length} characters"));
    }
    // 3-of-4 character classes, with a passphrase escape: a long enough
    // password is accepted on length alone (NIST SP 800-63B style).
    let has_upper = password.chars().any(|c| c.is_ascii_uppercase());
    let has_lower = password.chars().any(|c| c.is_ascii_lowercase());
    let has_digit = password.chars().any(|c| c.is_ascii_digit());
    let has_symbol = password.chars().any(|c| !c.is_ascii_alphanumeric());
    let classes = [has_upper, has_lower, has_digit, has_symbol]
        .iter()
        .filter(|b| **b)
        .count();
    if classes < 3 && chars < 20 {
        return Err(
            "Password must combine at least 3 of uppercase, lowercase, digits, symbols \
             — or be at least 20 characters (passphrase)"
                .to_string(),
        );
    }
    Ok(())
}
