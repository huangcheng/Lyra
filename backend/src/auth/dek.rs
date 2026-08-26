//! Master key and per-user DEK helpers.

use zeroize::Zeroizing;

use crate::crypto::{self, CryptoError};
use crate::db_row::{id_from_row, id_param};
use crate::storage::DbPool;
use sqlx::Row;

use super::totp::encrypt_totp_secret;
use super::types::AuthError;

// ── Master key & DEK hierarchy ──────────────────────────────────────
//
// Master key (from `LYRA_MASTER_KEY`, validated at boot in config.rs)
//   → per-user KEK (HKDF-SHA256, info string bound to the user id)
//   → wraps a random 256-bit DEK stored in `lyra_user.encrypted_dek`
//   → DEK encrypts account credentials and the TOTP secret.
//
// See `docs/specs/2026-08-20-lyra-data-model-spec.md` §3.

/// Process-wide master key, installed once at boot by [`AuthState::new`].
static MASTER_KEY: std::sync::OnceLock<Zeroizing<Vec<u8>>> = std::sync::OnceLock::new();

/// Install the master key. The first install wins; later calls are no-ops
/// (tests share one process-wide key). A second install with a *different*
/// key is suspicious, so it is logged.
pub(crate) fn install_master_key(key: &[u8]) {
    if MASTER_KEY.set(Zeroizing::new(key.to_vec())).is_err()
        && let Some(existing) = MASTER_KEY.get()
        && existing.as_slice() != key
    {
        tracing::warn!(
            "install_master_key called again with a different key; keeping the first (first install wins)"
        );
    }
}

pub(crate) fn master_key() -> Result<&'static [u8], CryptoError> {
    MASTER_KEY
        .get()
        .map(|k| k.as_slice())
        .ok_or(CryptoError::MasterKeyNotInitialized)
}

/// Fixed master key shared by all tests in this crate (32+ bytes).
#[cfg(test)]
pub(crate) const TEST_MASTER_KEY: &[u8] = b"lyra-test-master-key-0123456789abcdef";

/// Install [`TEST_MASTER_KEY`] as the process-wide master key (idempotent).
#[cfg(test)]
pub(crate) fn install_test_master_key() {
    install_master_key(TEST_MASTER_KEY);
}

/// Fetch the wrapped DEK blob from `lyra_user.encrypted_dek`.
pub(crate) async fn fetch_encrypted_dek(db: &DbPool, user_id: &str) -> Result<String, CryptoError> {
    let id = id_param(db, user_id).map_err(|e| CryptoError::Storage(e.to_string()))?;
    let wrapped: Option<String> = match db {
        DbPool::Sqlite(pool) => {
            sqlx::query_scalar("SELECT encrypted_dek FROM lyra_user WHERE id = ?")
                .bind(&id)
                .fetch_optional(pool)
                .await
        }
        #[cfg(feature = "postgres")]
        DbPool::Postgres(pool) => {
            sqlx::query_scalar("SELECT encrypted_dek FROM lyra_user WHERE id = $1")
                .bind(&id)
                .fetch_optional(pool)
                .await
        }
    }
    .map_err(|e| CryptoError::Storage(e.to_string()))?
    .flatten();
    wrapped.ok_or(CryptoError::MissingDek)
}

/// Persist a wrapped DEK only when the user exists and has none yet.
async fn store_wrapped_dek_if_missing(
    db: &DbPool,
    user_id: &str,
    wrapped: &str,
) -> Result<u64, CryptoError> {
    let id = id_param(db, user_id).map_err(|e| CryptoError::Storage(e.to_string()))?;
    db_execute!(
        db,
        "UPDATE lyra_user SET encrypted_dek = ?, updated_at = datetime('now') \
         WHERE id = ? AND encrypted_dek IS NULL",
        wrapped,
        &id
    )
    .map_err(|e| CryptoError::Storage(e.to_string()))
}

/// Mint a DEK for a pre-hierarchy user and re-encrypt secrets that still use
/// the padded-master-key scheme.
pub(crate) async fn provision_and_rotate_legacy_dek(
    db: &DbPool,
    user_id: &str,
    kek: &[u8; 32],
) -> Result<Vec<u8>, CryptoError> {
    let dek = crypto::generate_key();
    let wrapped = crypto::wrap_dek(kek, &dek)?;
    let wrote = store_wrapped_dek_if_missing(db, user_id, &wrapped).await?;
    if wrote == 0 {
        // Unknown user, or another caller stored a DEK first.
        let existing = fetch_encrypted_dek(db, user_id).await?;
        return crypto::unwrap_dek(kek, &existing);
    }
    if let Err(e) = rotate_legacy_secrets(db, user_id, &dek).await {
        tracing::error!(error = %e, %user_id, "failed to rotate legacy secrets onto the new DEK");
        return Err(e);
    }
    Ok(dek.to_vec())
}

async fn rotate_legacy_secrets(db: &DbPool, user_id: &str, dek: &[u8]) -> Result<(), CryptoError> {
    let master = master_key()?;
    let accounts = fetch_account_credentials(db, user_id).await?;
    for (account_id, credential) in accounts {
        let Ok(blob) = serde_json::from_str::<crypto::EncryptedCredential>(&credential) else {
            continue;
        };
        let Some(plaintext) = crypto::try_decrypt_with_legacy_keys(&blob, master) else {
            continue;
        };
        let plaintext = Zeroizing::new(plaintext);
        let rotated = crypto::encrypt(dek, &plaintext)?;
        let json =
            serde_json::to_string(&rotated).map_err(|e| CryptoError::Encrypt(e.to_string()))?;
        update_account_credential(db, &account_id, &json).await?;
    }
    rotate_legacy_totp_secret(db, user_id, dek, master).await?;
    Ok(())
}

async fn fetch_account_credentials(
    db: &DbPool,
    user_id: &str,
) -> Result<Vec<(String, String)>, CryptoError> {
    let id = id_param(db, user_id).map_err(|e| CryptoError::Storage(e.to_string()))?;
    db_fetch_all!(
        db,
        "SELECT id, credential FROM mail_account WHERE user_id = ?",
        |row| (id_from_row(row, "id"), row.get::<String, _>("credential")),
        &id
    )
    .map_err(|e| CryptoError::Storage(e.to_string()))
}

async fn update_account_credential(
    db: &DbPool,
    account_id: &str,
    credential: &str,
) -> Result<(), CryptoError> {
    let id = id_param(db, account_id).map_err(|e| CryptoError::Storage(e.to_string()))?;
    db_execute!(
        db,
        "UPDATE mail_account SET credential = ?, updated_at = datetime('now') WHERE id = ?",
        credential,
        &id
    )
    .map_err(|e| CryptoError::Storage(e.to_string()))?;
    Ok(())
}

async fn rotate_legacy_totp_secret(
    db: &DbPool,
    user_id: &str,
    dek: &[u8],
    master: &[u8],
) -> Result<(), CryptoError> {
    let id = id_param(db, user_id).map_err(|e| CryptoError::Storage(e.to_string()))?;
    let stored: Option<Option<String>> = db_scalar_optional!(
        db,
        Option<String>,
        "SELECT totp_secret FROM lyra_user WHERE id = ?",
        &id
    )
    .map_err(|e| CryptoError::Storage(e.to_string()))?;
    let Some(stored) = stored.flatten() else {
        return Ok(());
    };
    let plaintext = if let Ok(blob) = serde_json::from_str::<crypto::EncryptedCredential>(&stored) {
        let Some(pt) = crypto::try_decrypt_with_legacy_keys(&blob, master) else {
            return Ok(());
        };
        Zeroizing::new(String::from_utf8_lossy(&pt).into_owned())
    } else {
        Zeroizing::new(stored)
    };
    let rotated = encrypt_totp_secret(dek, &plaintext)?;
    db_execute!(
        db,
        "UPDATE lyra_user SET totp_secret = ? WHERE id = ?",
        &rotated,
        &id
    )
    .map_err(|e| CryptoError::Storage(e.to_string()))?;
    Ok(())
}

/// Map a crypto failure to a 500. The error text carries deliberate operator
/// guidance (never key material or plaintext).
pub(crate) fn crypto_err(e: CryptoError) -> AuthError {
    tracing::error!("crypto failure: {e}");
    AuthError::Crypto(e)
}

// ── Application state ───────────────────────────────────────────────
