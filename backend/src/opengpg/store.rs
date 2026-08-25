//! Persist OpenGPG keys (armored, passphrase-locked secrets at rest).
//!
//! Wired to HTTP in CHE-61; keep the seam public and tested.

#![allow(dead_code)]

use sqlx::Row;
use uuid::Uuid;

use super::keys::{OpengpgError, ParsedKey, parse_armored_key};
use crate::db_row::{id_from_row, id_param, json_text_from_row, opt_json_param, opt_ts_from_row};
use crate::storage::DbPool;

/// Stored key row (never includes unlocked secret material).
#[derive(Debug, Clone)]
#[allow(dead_code)] // fields reserved for CHE-61 API responses
pub struct StoredKey {
    pub id: String,
    pub user_id: String,
    pub fingerprint: String,
    pub primary_email: String,
    pub emails: Vec<String>,
    pub is_secret: bool,
    pub is_primary: bool,
    pub revoked: bool,
    /// Armored key data (secret keys stay passphrase-locked).
    pub key_data: String,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
}

/// Insert a newly parsed key for `user_id`.
pub async fn insert_key(
    db: &DbPool,
    user_id: &str,
    parsed: &ParsedKey,
    is_primary: bool,
) -> Result<StoredKey, OpengpgError> {
    let id = Uuid::new_v7(uuid::Timestamp::now(uuid::NoContext)).to_string();
    let emails_json = serde_json::to_string(&parsed.emails)
        .map_err(|e| OpengpgError::InvalidInput(e.to_string()))?;
    let user_bind =
        id_param(db, user_id).map_err(|_| OpengpgError::InvalidInput("user id".into()))?;
    let id_bind = id_param(db, &id).map_err(|_| OpengpgError::InvalidInput("key id".into()))?;
    let emails_bind = opt_json_param(db, Some(emails_json.as_str()));

    if is_primary {
        db_execute!(
            db,
            "UPDATE opengpg_key SET is_primary = ?, updated_at = datetime('now') WHERE user_id = ?",
            false,
            &user_bind
        )?;
    }

    db_execute!(
        db,
        r"
        INSERT INTO opengpg_key (
            id, user_id, fingerprint, primary_email, emails,
            is_secret, is_primary, revoked, key_data
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
        ",
        &id_bind,
        &user_bind,
        &parsed.fingerprint,
        &parsed.primary_email,
        &emails_bind,
        parsed.is_secret,
        is_primary,
        parsed.revoked,
        &parsed.key_data
    )?;

    get_key(db, user_id, &id)
        .await?
        .ok_or(OpengpgError::NotFound)
}

/// Parse armored input and insert.
pub async fn import_armored(
    db: &DbPool,
    user_id: &str,
    armored: &str,
    is_primary: bool,
) -> Result<StoredKey, OpengpgError> {
    let parsed = parse_armored_key(armored)?;
    insert_key(db, user_id, &parsed, is_primary).await
}

pub async fn list_keys(db: &DbPool, user_id: &str) -> Result<Vec<StoredKey>, OpengpgError> {
    let user_bind =
        id_param(db, user_id).map_err(|_| OpengpgError::InvalidInput("user id".into()))?;
    let rows = db_fetch_all!(
        db,
        r"
        SELECT id, user_id, fingerprint, primary_email, emails,
               is_secret, is_primary, revoked, key_data, created_at, updated_at
        FROM opengpg_key
        WHERE user_id = ?
        ORDER BY is_primary DESC, primary_email ASC
        ",
        |row| {
            let emails_raw = json_text_from_row(&row, "emails").unwrap_or_else(|| "[]".into());
            let emails: Vec<String> = serde_json::from_str(&emails_raw).unwrap_or_default();
            StoredKey {
                id: id_from_row(&row, "id"),
                user_id: id_from_row(&row, "user_id"),
                fingerprint: row.get("fingerprint"),
                primary_email: row.get("primary_email"),
                emails,
                is_secret: row.get("is_secret"),
                is_primary: row.get("is_primary"),
                revoked: row.get("revoked"),
                key_data: row.get("key_data"),
                created_at: opt_ts_from_row(&row, "created_at"),
                updated_at: opt_ts_from_row(&row, "updated_at"),
            }
        },
        &user_bind
    )?;
    Ok(rows)
}

pub async fn get_key(
    db: &DbPool,
    user_id: &str,
    key_id: &str,
) -> Result<Option<StoredKey>, OpengpgError> {
    let user_bind =
        id_param(db, user_id).map_err(|_| OpengpgError::InvalidInput("user id".into()))?;
    let key_bind = id_param(db, key_id).map_err(|_| OpengpgError::InvalidInput("key id".into()))?;
    let row = db_fetch_optional!(
        db,
        r"
        SELECT id, user_id, fingerprint, primary_email, emails,
               is_secret, is_primary, revoked, key_data, created_at, updated_at
        FROM opengpg_key
        WHERE id = ? AND user_id = ?
        ",
        |row| {
            let emails_raw = json_text_from_row(&row, "emails").unwrap_or_else(|| "[]".into());
            let emails: Vec<String> = serde_json::from_str(&emails_raw).unwrap_or_default();
            StoredKey {
                id: id_from_row(&row, "id"),
                user_id: id_from_row(&row, "user_id"),
                fingerprint: row.get("fingerprint"),
                primary_email: row.get("primary_email"),
                emails,
                is_secret: row.get("is_secret"),
                is_primary: row.get("is_primary"),
                revoked: row.get("revoked"),
                key_data: row.get("key_data"),
                created_at: opt_ts_from_row(&row, "created_at"),
                updated_at: opt_ts_from_row(&row, "updated_at"),
            }
        },
        &key_bind,
        &user_bind
    )?;
    Ok(row)
}

/// Return armored key_data for export (caller enforces re-auth for secrets).
pub async fn export_armored(
    db: &DbPool,
    user_id: &str,
    key_id: &str,
) -> Result<String, OpengpgError> {
    let key = get_key(db, user_id, key_id)
        .await?
        .ok_or(OpengpgError::NotFound)?;
    Ok(key.key_data)
}

pub async fn delete_key(db: &DbPool, user_id: &str, key_id: &str) -> Result<(), OpengpgError> {
    let existing = get_key(db, user_id, key_id)
        .await?
        .ok_or(OpengpgError::NotFound)?;
    if existing.is_primary {
        return Err(OpengpgError::InvalidInput(
            "refuse deleting primary key; promote another first".into(),
        ));
    }
    let user_bind =
        id_param(db, user_id).map_err(|_| OpengpgError::InvalidInput("user id".into()))?;
    let key_bind = id_param(db, key_id).map_err(|_| OpengpgError::InvalidInput("key id".into()))?;
    db_execute!(
        db,
        "DELETE FROM opengpg_key WHERE id = ? AND user_id = ?",
        &key_bind,
        &user_bind
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::opengpg::keys::tests_support::gen_test_secret_armor;
    use crate::storage::create_test_state;
    use uuid::Uuid;

    #[tokio::test]
    async fn import_list_export_roundtrip() {
        let state = create_test_state().await;
        let db = &state.db;
        let user_id = Uuid::new_v7(uuid::Timestamp::now(uuid::NoContext)).to_string();
        db_execute!(
            db,
            "INSERT INTO lyra_user (id, username, password_hash) VALUES (?, ?, ?)",
            &id_param(db, &user_id).unwrap(),
            "opengpg-tester",
            "hash"
        )
        .unwrap();

        let armor = gen_test_secret_armor(Some("s3cret"));
        let stored = import_armored(db, &user_id, &armor, true)
            .await
            .expect("import");
        assert!(stored.is_secret);
        assert!(stored.is_primary);
        assert_eq!(stored.primary_email, "test@example.com");

        let listed = list_keys(db, &user_id).await.expect("list");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].fingerprint, stored.fingerprint);

        let exported = export_armored(db, &user_id, &stored.id)
            .await
            .expect("export");
        assert_eq!(exported, stored.key_data);

        // Primary delete refused
        let err = delete_key(db, &user_id, &stored.id).await.unwrap_err();
        assert!(matches!(err, OpengpgError::InvalidInput(_)));
    }
}
