//! Sync failure recovery helpers (§10 failure matrix).
//!
//! Credential decrypt failures deactivate the account so the UI can prompt
//! for re-entry. Oversized bodies are skipped and flagged `fetch_error`.

#![allow(clippy::doc_markdown)]

use super::types::SyncError;
use crate::db_row::id_param;
use crate::storage::DbPool;

/// Soft cap for lazy BODY fetch (25 MiB). Larger messages are skipped.
pub(crate) const MAX_MESSAGE_BODY_BYTES: u64 = 25 * 1024 * 1024;

/// Mark a mail account inactive (credential / auth recovery path).
pub(crate) async fn deactivate_account(db: &DbPool, account_id: &str) -> Result<(), SyncError> {
    db_execute!(
        db,
        r"
        UPDATE mail_account
        SET is_active = ?, sync_enabled = ?, updated_at = datetime('now')
        WHERE id = ?
        ",
        false,
        false,
        &id_param(db, account_id)?
    )?;
    tracing::warn!(
        account_id,
        "deactivated mail account after credential failure"
    );
    Ok(())
}

/// Decrypt failure → deactivate account and return a secret-free [`SyncError::Crypto`].
pub(crate) async fn fail_credential_decrypt(db: &DbPool, account_id: &str) -> SyncError {
    if let Err(error) = deactivate_account(db, account_id).await {
        tracing::error!(account_id, %error, "failed to deactivate account after decrypt error");
    }
    SyncError::Crypto("credential decrypt failed; re-enter account password".into())
}

/// Record a per-message fetch failure in `flags.fetch_error` without aborting sync.
pub(crate) async fn mark_message_fetch_error(
    db: &DbPool,
    message_id: &str,
    reason: &str,
) -> Result<(), SyncError> {
    let existing: Option<String> = db_scalar_optional!(
        db,
        String,
        "SELECT flags FROM message WHERE id = ?",
        &id_param(db, message_id)?
    )?;

    let mut map = existing
        .as_deref()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();
    map.insert(
        "fetch_error".into(),
        serde_json::Value::String(reason.to_owned()),
    );
    let flags = serde_json::to_string(&map).unwrap_or_else(|_| {
        format!(
            r#"{{"fetch_error":{}}}"#,
            serde_json::to_string(reason).unwrap_or_default()
        )
    });

    db_execute!(
        db,
        r"
        UPDATE message
        SET flags = ?, updated_at = datetime('now')
        WHERE id = ?
        ",
        &flags,
        &id_param(db, message_id)?
    )?;
    Ok(())
}

/// True when `size_bytes` exceeds the lazy-fetch soft cap.
#[must_use]
pub(crate) fn body_exceeds_limit(size_bytes: Option<i64>) -> bool {
    match size_bytes {
        Some(s) if s > 0 => u64::try_from(s).is_ok_and(|n| n > MAX_MESSAGE_BODY_BYTES),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn body_limit_rejects_oversize() {
        assert!(!body_exceeds_limit(None));
        assert!(!body_exceeds_limit(Some(1024)));
        assert!(body_exceeds_limit(Some(
            i64::try_from(MAX_MESSAGE_BODY_BYTES + 1).unwrap()
        )));
    }
}
