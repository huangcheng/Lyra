//! Sync engine module.
//!
//! Orchestrates IMAP adapters, writes to storage, and tracks sync state
//! via the `sync_cursor` table for idempotent, resumable sync.
//!
//! See `docs/specs/2026-08-20-lyra-sync-and-protocols-spec.md`.

#![allow(clippy::doc_markdown)]

use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode, header},
    response::IntoResponse,
    routing::{get, post},
};
use serde::Serialize;
use sqlx::Row;
use uuid::Uuid;

use crate::auth::AuthState;
use crate::imap::{ImapClient, ImapConfig, ImapError, ImapMessage, ImapSecurity};
use crate::storage::DbPool;

// ── API types ───────────────────────────────────────────────────────

/// Response for sync status endpoint.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncStatus {
    pub active_accounts: i64,
    pub syncing: bool,
}

/// Response for a sync operation.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncResponse {
    pub account_id: String,
    pub status: String,
    pub folders_synced: usize,
    pub messages_synced: usize,
    pub messages_updated: usize,
    pub messages_deleted: usize,
}

/// Error type for sync operations.
#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error("account not found")]
    AccountNotFound,
    #[error("account not active or sync disabled")]
    AccountDisabled,
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("imap error: {0}")]
    Imap(#[from] ImapError),
    #[error("authentication required")]
    Unauthorized,
    #[error("crypto error: {0}")]
    Crypto(String),
}

impl IntoResponse for SyncError {
    fn into_response(self) -> axum::response::Response {
        let status = match self {
            SyncError::AccountNotFound => StatusCode::NOT_FOUND,
            SyncError::AccountDisabled => StatusCode::BAD_REQUEST,
            SyncError::Database(_) | SyncError::Crypto(_) => StatusCode::INTERNAL_SERVER_ERROR,
            SyncError::Imap(_) => StatusCode::BAD_GATEWAY,
            SyncError::Unauthorized => StatusCode::UNAUTHORIZED,
        };
        (
            status,
            Json(serde_json::json!({ "error": self.to_string() })),
        )
            .into_response()
    }
}

// ── Routes ──────────────────────────────────────────────────────────

/// Routes for sync-related endpoints.
pub fn routes() -> Router<AuthState> {
    Router::new()
        .route("/api/sync/status", get(sync_status))
        .route("/api/accounts/{account_id}/sync", post(trigger_sync))
}

// ── Handlers ────────────────────────────────────────────────────────

/// Get sync status.
async fn sync_status(
    State(state): State<AuthState>,
    headers: HeaderMap,
) -> Result<Json<SyncStatus>, SyncError> {
    let user_id = get_user_id(&state, &headers).await?;
    let pool = get_sqlite_pool(state.db());

    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM mail_account WHERE user_id = ? AND is_active = 1 AND sync_enabled = 1",
    )
    .bind(&user_id)
    .fetch_one(pool)
    .await?;

    Ok(Json(SyncStatus {
        active_accounts: count,
        syncing: false, // v1: synchronous sync, never "in progress"
    }))
}

/// Trigger a sync for a specific account.
async fn trigger_sync(
    State(state): State<AuthState>,
    Path(account_id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<SyncResponse>, SyncError> {
    let user_id = get_user_id(&state, &headers).await?;

    let result = run_account_sync(&state.db, &user_id, &account_id).await?;

    Ok(Json(result))
}

// ── Sync orchestration ──────────────────────────────────────────────

/// Run a full sync for a mail account.
///
/// 1. Load account + decrypt credentials
/// 2. Connect to IMAP server
/// 3. Sync folders (idempotent upsert)
/// 4. For each folder: sync messages using UIDVALIDITY + UID cursor
/// 5. Update `last_sync_at` on the account
#[allow(clippy::too_many_lines)]
pub async fn run_account_sync(
    db: &DbPool,
    user_id: &str,
    account_id: &str,
) -> Result<SyncResponse, SyncError> {
    let pool = get_sqlite_pool(db);

    // 1. Load account
    let row = sqlx::query(
        r"
        SELECT id, email_address, protocol, credential,
               imap_host, imap_port, imap_security, is_active, sync_enabled
        FROM mail_account
        WHERE id = ? AND user_id = ?
        ",
    )
    .bind(account_id)
    .bind(user_id)
    .fetch_optional(pool)
    .await?
    .ok_or(SyncError::AccountNotFound)?;

    let is_active: bool = row.get("is_active");
    let sync_enabled: bool = row.get("sync_enabled");

    if !is_active || !sync_enabled {
        return Err(SyncError::AccountDisabled);
    }

    let protocol: String = row.get("protocol");
    if protocol != "imap" {
        return Err(SyncError::AccountNotFound); // Only IMAP supported for now
    }

    let imap_host: Option<String> = row.get("imap_host");
    let imap_port: Option<i32> = row.get("imap_port");
    let imap_security: Option<String> = row.get("imap_security");
    let credential_json: String = row.get("credential");
    let email_address: String = row.get("email_address");

    let host = imap_host.ok_or_else(|| SyncError::Crypto("IMAP host not configured".into()))?;
    let port = u16::try_from(imap_port.unwrap_or(993)).unwrap_or(993);
    let security = match imap_security.as_deref() {
        Some("starttls") => ImapSecurity::Starttls,
        Some("none") => ImapSecurity::None,
        _ => ImapSecurity::Tls,
    };

    // 2. Decrypt password
    let dek =
        crate::auth::AuthState::get_user_dek().map_err(|e| SyncError::Crypto(e.to_string()))?;
    let password = crate::imap::decrypt_account_password(&credential_json, &dek)?;

    let config = ImapConfig {
        host,
        port,
        security,
        username: email_address,
        password,
    };

    // 3. Connect to IMAP
    let mut client = ImapClient::connect(&config).await?;

    // 4. Sync folders
    let folders = client.list_folders().await?;
    let mut folders_synced = 0;

    for folder in &folders {
        upsert_folder(pool, account_id, &folder.name, folder.delimiter.as_deref()).await?;
        folders_synced += 1;
    }

    // 5. Sync messages in each folder
    let mut total_new = 0;
    let mut total_updated = 0;
    let total_deleted = 0;

    for folder in &folders {
        let folder_id = get_folder_id(pool, account_id, &folder.name).await?;

        let (uid_validity, _uid_next, _exists) = client.select(&folder.name).await?;

        // Load existing cursor
        let cursor = load_cursor(pool, account_id, &folder_id).await?;

        // Check if UIDVALIDITY changed — if so, we need a full resync
        let after_uid = if let Some(ref c) = cursor {
            if c.uid_validity == uid_validity {
                Some(c.last_uid)
            } else {
                // UIDVALIDITY changed: clear old messages for this folder and resync
                clear_folder_messages(pool, &folder_id).await?;
                None
            }
        } else {
            None
        };

        // Fetch UIDs after the cursor
        let uids = client.search_uids(after_uid).await?;

        if uids.is_empty() {
            continue;
        }

        // Fetch metadata for all new/updated UIDs
        let messages = client.fetch_metadata(&uids).await?;

        // Upsert each message
        for msg in &messages {
            let was_new = upsert_message(pool, account_id, &folder_id, msg).await?;
            if was_new {
                total_new += 1;
            } else {
                total_updated += 1;
            }
        }

        // Update cursor to the highest UID we've seen
        let max_uid = messages.iter().map(|m| m.uid).max().unwrap_or(0);
        let new_cursor_uid = std::cmp::max(max_uid, cursor.map_or(0, |c| c.last_uid));
        save_cursor(
            pool,
            account_id,
            &folder_id,
            "imap",
            uid_validity,
            new_cursor_uid,
        )
        .await?;

        // Update folder counts
        update_folder_counts(pool, &folder_id).await?;
    }

    // 6. Update last_sync_at
    sqlx::query("UPDATE mail_account SET last_sync_at = datetime('now'), updated_at = datetime('now') WHERE id = ?")
        .bind(account_id)
        .execute(pool)
        .await?;

    // Clean logout
    let _ = client.logout().await;

    Ok(SyncResponse {
        account_id: account_id.to_string(),
        status: "completed".to_string(),
        folders_synced,
        messages_synced: total_new,
        messages_updated: total_updated,
        messages_deleted: total_deleted,
    })
}

// ── Database helpers ────────────────────────────────────────────────

fn get_sqlite_pool(db: &DbPool) -> &sqlx::SqlitePool {
    match db {
        DbPool::Sqlite(pool) => pool,
        #[cfg(feature = "postgres")]
        DbPool::Postgres(_) => panic!("PostgreSQL not supported yet"),
    }
}

async fn get_user_id(state: &AuthState, headers: &HeaderMap) -> Result<String, SyncError> {
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or(SyncError::Unauthorized)?;

    state
        .sessions
        .get_session(token)
        .await
        .ok_or(SyncError::Unauthorized)
}

/// Upsert a folder by `(account_id, name)`.
///
/// Returns `Ok(())` on success. Uses the `external_id` field to store
/// the IMAP folder name for uniqueness.
async fn upsert_folder(
    pool: &sqlx::SqlitePool,
    account_id: &str,
    name: &str,
    _delimiter: Option<&str>,
) -> Result<(), SyncError> {
    let role = infer_folder_role(name);
    let external_id = name;

    // Try to find existing folder
    let existing: Option<String> =
        sqlx::query_scalar("SELECT id FROM folder WHERE account_id = ? AND external_id = ?")
            .bind(account_id)
            .bind(external_id)
            .fetch_optional(pool)
            .await?;

    if let Some(id) = existing {
        // Update
        sqlx::query("UPDATE folder SET name = ?, updated_at = datetime('now') WHERE id = ?")
            .bind(name)
            .bind(&id)
            .execute(pool)
            .await?;
    } else {
        // Insert
        let id = Uuid::new_v7(uuid::Timestamp::now(uuid::NoContext)).to_string();
        sqlx::query(
            r"
            INSERT INTO folder (id, account_id, external_id, name, role, sort_order)
            VALUES (?, ?, ?, ?, ?, 0)
            ",
        )
        .bind(&id)
        .bind(account_id)
        .bind(external_id)
        .bind(name)
        .bind(role)
        .execute(pool)
        .await?;
    }

    Ok(())
}

/// Get the local folder ID by `external_id`.
async fn get_folder_id(
    pool: &sqlx::SqlitePool,
    account_id: &str,
    name: &str,
) -> Result<String, SyncError> {
    let id: String =
        sqlx::query_scalar("SELECT id FROM folder WHERE account_id = ? AND external_id = ?")
            .bind(account_id)
            .bind(name)
            .fetch_optional(pool)
            .await?
            .ok_or_else(|| SyncError::Database(sqlx::Error::RowNotFound))?;

    Ok(id)
}

/// Infer folder role from name.
fn infer_folder_role(name: &str) -> Option<&'static str> {
    let lower = name.to_lowercase();
    if lower == "inbox" {
        Some("inbox")
    } else if lower == "sent" || lower == "sent mail" || lower == "sent messages" {
        Some("sent")
    } else if lower == "drafts" || lower == "draft" {
        Some("drafts")
    } else if lower == "trash" || lower == "deleted" || lower == "deleted messages" {
        Some("trash")
    } else if lower == "spam" || lower == "junk" || lower == "bulk mail" {
        Some("spam")
    } else if lower == "archive" || lower == "all mail" || lower == "all messages" {
        Some("archive")
    } else {
        None
    }
}

// ── Sync cursor ─────────────────────────────────────────────────────

/// Stored sync cursor for a folder.
struct SyncCursorInfo {
    uid_validity: u32,
    last_uid: u32,
}

/// Load the sync cursor for a folder.
async fn load_cursor(
    pool: &sqlx::SqlitePool,
    account_id: &str,
    folder_id: &str,
) -> Result<Option<SyncCursorInfo>, SyncError> {
    let row = sqlx::query(
        r"
        SELECT cursor_value
        FROM sync_cursor
        WHERE account_id = ? AND folder_id = ? AND cursor_type = 'uidvalidity_uid'
        ",
    )
    .bind(account_id)
    .bind(folder_id)
    .fetch_optional(pool)
    .await?;

    Ok(row.map(|r| {
        let value: String = r.get("cursor_value");
        parse_cursor_value(&value)
    }))
}

/// Save the sync cursor for a folder.
///
/// Cursor format: `{uid_validity}:{last_uid}` for the `uidvalidity_uid` type.
async fn save_cursor(
    pool: &sqlx::SqlitePool,
    account_id: &str,
    folder_id: &str,
    protocol: &str,
    uid_validity: u32,
    last_uid: u32,
) -> Result<(), SyncError> {
    let cursor_value = format!("{uid_validity}:{last_uid}");
    let cursor_id = format!("{account_id}:{folder_id}:uidvalidity_uid");

    sqlx::query(
        r"
        INSERT INTO sync_cursor (id, account_id, folder_id, protocol, cursor_type, cursor_value, updated_at)
        VALUES (?, ?, ?, ?, 'uidvalidity_uid', ?, datetime('now'))
        ON CONFLICT(account_id, folder_id, cursor_type)
        DO UPDATE SET cursor_value = excluded.cursor_value, updated_at = excluded.updated_at
        ",
    )
    .bind(&cursor_id)
    .bind(account_id)
    .bind(folder_id)
    .bind(protocol)
    .bind(&cursor_value)
    .execute(pool)
    .await?;

    Ok(())
}

/// Parse a cursor value string `{uid_validity}:{last_uid}`.
fn parse_cursor_value(value: &str) -> SyncCursorInfo {
    let parts: Vec<&str> = value.splitn(2, ':').collect();
    if parts.len() == 2 {
        SyncCursorInfo {
            uid_validity: parts[0].parse().unwrap_or(0),
            last_uid: parts[1].parse().unwrap_or(0),
        }
    } else {
        SyncCursorInfo {
            uid_validity: 0,
            last_uid: 0,
        }
    }
}

// ── Message upsert ──────────────────────────────────────────────────

/// Upsert a message from IMAP metadata.
///
/// Returns `true` if the message was newly inserted, `false` if updated.
async fn upsert_message(
    pool: &sqlx::SqlitePool,
    account_id: &str,
    folder_id: &str,
    msg: &ImapMessage,
) -> Result<bool, SyncError> {
    let external_id = msg.uid.to_string();

    // Check if message already exists
    let existing: Option<String> =
        sqlx::query_scalar("SELECT id FROM message WHERE account_id = ? AND external_id = ?")
            .bind(account_id)
            .bind(&external_id)
            .fetch_optional(pool)
            .await?;

    let flags_json = serde_json::to_string(&msg.flags).unwrap_or_else(|_| "{}".into());
    let is_read = msg
        .flags
        .iter()
        .any(|f| f.contains("Seen") || f.contains("\\Seen"));
    let is_starred = msg
        .flags
        .iter()
        .any(|f| f.contains("Flagged") || f.contains("\\Flagged"));
    let snippet = msg.subject.as_deref().map(|s| {
        if s.len() > 120 {
            format!("{}...", &s[..117])
        } else {
            s.to_string()
        }
    });

    let from_json = msg
        .from
        .as_ref()
        .map(|f| serde_json::json!({ "raw": f }).to_string());
    let to_json = msg
        .to
        .as_ref()
        .map(|t| serde_json::json!(vec![t]).to_string());

    if let Some(id) = existing {
        // Update flags
        sqlx::query(
            r"
            UPDATE message SET
                is_read = ?,
                is_starred = ?,
                flags = ?,
                updated_at = datetime('now')
            WHERE id = ?
            ",
        )
        .bind(is_read)
        .bind(is_starred)
        .bind(&flags_json)
        .bind(&id)
        .execute(pool)
        .await?;

        Ok(false)
    } else {
        // Insert new message
        let id = Uuid::new_v7(uuid::Timestamp::now(uuid::NoContext)).to_string();

        sqlx::query(
            r"
            INSERT INTO message (
                id, account_id, folder_id, external_id,
                message_id_header, subject, from_address, to_addresses,
                cc_addresses, date, is_read, is_starred,
                flags, size_bytes, in_reply_to, references_headers,
                snippet, has_attachments, body_text, body_html
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            ",
        )
        .bind(&id)
        .bind(account_id)
        .bind(folder_id)
        .bind(&external_id)
        .bind(&msg.message_id)
        .bind(&msg.subject)
        .bind(&from_json)
        .bind(&to_json)
        .bind(&msg.cc)
        .bind(&msg.date)
        .bind(is_read)
        .bind(is_starred)
        .bind(&flags_json)
        .bind(msg.size.map(i32::try_from).transpose().unwrap_or(None))
        .bind(&msg.in_reply_to)
        .bind(&msg.references)
        .bind(&snippet)
        .bind(msg.has_attachments)
        .bind(&msg.body_text)
        .bind(&msg.body_html)
        .execute(pool)
        .await?;

        Ok(true)
    }
}

/// Delete all messages in a folder (used when UIDVALIDITY changes).
async fn clear_folder_messages(pool: &sqlx::SqlitePool, folder_id: &str) -> Result<(), SyncError> {
    sqlx::query("DELETE FROM message WHERE folder_id = ?")
        .bind(folder_id)
        .execute(pool)
        .await?;

    // Also clear the cursor
    sqlx::query("DELETE FROM sync_cursor WHERE folder_id = ?")
        .bind(folder_id)
        .execute(pool)
        .await?;

    Ok(())
}

/// Update folder message counts from the message table.
async fn update_folder_counts(pool: &sqlx::SqlitePool, folder_id: &str) -> Result<(), SyncError> {
    let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM message WHERE folder_id = ?")
        .bind(folder_id)
        .fetch_one(pool)
        .await?;

    let unread: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM message WHERE folder_id = ? AND is_read = 0")
            .bind(folder_id)
            .fetch_one(pool)
            .await?;

    sqlx::query(
        "UPDATE folder SET total_messages = ?, unread_messages = ?, updated_at = datetime('now') WHERE id = ?",
    )
    .bind(i32::try_from(total).unwrap_or(i32::MAX))
    .bind(i32::try_from(unread).unwrap_or(i32::MAX))
    .bind(folder_id)
    .execute(pool)
    .await?;

    Ok(())
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::Storage;

    /// Create an in-memory SQLite pool with migrations applied.
    async fn test_pool() -> sqlx::SqlitePool {
        let storage = Storage::new("sqlite::memory:").await.unwrap();
        storage.run_migrations().await.unwrap();
        match storage.pool().clone() {
            DbPool::Sqlite(pool) => pool,
            #[cfg(feature = "postgres")]
            DbPool::Postgres(_) => panic!("expected sqlite"),
        }
    }

    /// Seed a user and account in the test DB, return `(user_id, account_id)`.
    async fn seed_user_and_account(pool: &sqlx::SqlitePool) -> (String, String) {
        let user_id = Uuid::new_v7(uuid::Timestamp::now(uuid::NoContext)).to_string();
        let account_id = Uuid::new_v7(uuid::Timestamp::now(uuid::NoContext)).to_string();

        // Insert user
        sqlx::query("INSERT INTO lyra_user (id, username, password_hash) VALUES (?, ?, ?)")
            .bind(&user_id)
            .bind("testuser")
            .bind("hash")
            .execute(pool)
            .await
            .unwrap();

        // Insert account
        let dek = crate::auth::AuthState::get_user_dek().unwrap();
        let encrypted = crate::crypto::encrypt(&dek, b"password123").unwrap();
        let credential_json = serde_json::to_string(&encrypted).unwrap();

        sqlx::query(
            r"
            INSERT INTO mail_account (
                id, user_id, display_name, email_address, protocol, auth_type,
                credential, imap_host, imap_port, imap_security,
                is_active, sync_enabled
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 1, 1)
            ",
        )
        .bind(&account_id)
        .bind(&user_id)
        .bind("Test Account")
        .bind("test@example.com")
        .bind("imap")
        .bind("password")
        .bind(&credential_json)
        .bind("imap.example.com")
        .bind(993)
        .bind("tls")
        .execute(pool)
        .await
        .unwrap();

        (user_id, account_id)
    }

    #[test]
    fn infer_folder_role_standard() {
        assert_eq!(infer_folder_role("INBOX"), Some("inbox"));
        assert_eq!(infer_folder_role("Sent"), Some("sent"));
        assert_eq!(infer_folder_role("Sent Mail"), Some("sent"));
        assert_eq!(infer_folder_role("Drafts"), Some("drafts"));
        assert_eq!(infer_folder_role("Trash"), Some("trash"));
        assert_eq!(infer_folder_role("Spam"), Some("spam"));
        assert_eq!(infer_folder_role("Junk"), Some("spam"));
        assert_eq!(infer_folder_role("Archive"), Some("archive"));
        assert_eq!(infer_folder_role("All Mail"), Some("archive"));
    }

    #[test]
    fn infer_folder_role_custom() {
        assert_eq!(infer_folder_role("Projects"), None);
        assert_eq!(infer_folder_role("Lists/Rust"), None);
    }

    #[test]
    fn parse_cursor_value_valid() {
        let cursor = parse_cursor_value("12345:678");
        assert_eq!(cursor.uid_validity, 12345);
        assert_eq!(cursor.last_uid, 678);
    }

    #[test]
    fn parse_cursor_value_invalid() {
        let cursor = parse_cursor_value("garbage");
        assert_eq!(cursor.uid_validity, 0);
        assert_eq!(cursor.last_uid, 0);
    }

    #[tokio::test]
    async fn upsert_folder_insert_and_update() {
        let pool = test_pool().await;
        let (_, account_id) = seed_user_and_account(&pool).await;

        // First insert
        upsert_folder(&pool, &account_id, "INBOX", Some("/"))
            .await
            .unwrap();

        let id1 = get_folder_id(&pool, &account_id, "INBOX").await.unwrap();

        // Second upsert (update) — should not create a new row
        upsert_folder(&pool, &account_id, "INBOX", Some("/"))
            .await
            .unwrap();

        let id2 = get_folder_id(&pool, &account_id, "INBOX").await.unwrap();

        assert_eq!(id1, id2, "upsert should be idempotent");

        // Verify row count
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM folder WHERE account_id = ?")
            .bind(&account_id)
            .fetch_one(&pool)
            .await
            .unwrap();

        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn upsert_message_idempotent() {
        let pool = test_pool().await;
        let (_, account_id) = seed_user_and_account(&pool).await;

        // Create a folder first
        upsert_folder(&pool, &account_id, "INBOX", Some("/"))
            .await
            .unwrap();
        let folder_id = get_folder_id(&pool, &account_id, "INBOX").await.unwrap();

        let msg = ImapMessage {
            uid: 42,
            message_id: Some("<msg42@example.com>".into()),
            subject: Some("Test Subject".into()),
            from: Some("sender@example.com".into()),
            to: Some("test@example.com".into()),
            cc: None,
            date: Some("2025-01-15T10:00:00Z".into()),
            in_reply_to: None,
            references: None,
            flags: vec!["\\Seen".into()],
            size: Some(1234),
            body: None,
            body_text: None,
            body_html: None,
            has_attachments: false,
        };

        // First insert
        let was_new = upsert_message(&pool, &account_id, &folder_id, &msg)
            .await
            .unwrap();
        assert!(was_new, "first insert should return true");

        // Second upsert (update flags)
        let mut msg2 = msg.clone();
        msg2.flags = vec!["\\Seen".into(), "\\Flagged".into()];
        let was_new2 = upsert_message(&pool, &account_id, &folder_id, &msg2)
            .await
            .unwrap();
        assert!(!was_new2, "second upsert should return false (update)");

        // Verify only one row
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM message WHERE account_id = ? AND external_id = '42'",
        )
        .bind(&account_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(count, 1, "should have exactly one message row");

        // Verify flags were updated
        let flags: String = sqlx::query_scalar(
            "SELECT flags FROM message WHERE account_id = ? AND external_id = '42'",
        )
        .bind(&account_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(flags.contains("Flagged"), "flags should be updated");
    }

    #[tokio::test]
    async fn save_and_load_cursor() {
        let pool = test_pool().await;
        let (_, account_id) = seed_user_and_account(&pool).await;

        upsert_folder(&pool, &account_id, "INBOX", Some("/"))
            .await
            .unwrap();
        let folder_id = get_folder_id(&pool, &account_id, "INBOX").await.unwrap();

        // Save cursor
        save_cursor(&pool, &account_id, &folder_id, "imap", 12345, 100)
            .await
            .unwrap();

        // Load cursor
        let cursor = load_cursor(&pool, &account_id, &folder_id)
            .await
            .unwrap()
            .expect("cursor should exist");

        assert_eq!(cursor.uid_validity, 12345);
        assert_eq!(cursor.last_uid, 100);

        // Update cursor (idempotent upsert)
        save_cursor(&pool, &account_id, &folder_id, "imap", 12345, 200)
            .await
            .unwrap();

        let cursor2 = load_cursor(&pool, &account_id, &folder_id)
            .await
            .unwrap()
            .expect("cursor should exist");

        assert_eq!(cursor2.last_uid, 200, "cursor should be updated");

        // Verify only one cursor row
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM sync_cursor WHERE account_id = ?")
                .bind(&account_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn clear_folder_messages_works() {
        let pool = test_pool().await;
        let (_, account_id) = seed_user_and_account(&pool).await;

        upsert_folder(&pool, &account_id, "INBOX", Some("/"))
            .await
            .unwrap();
        let folder_id = get_folder_id(&pool, &account_id, "INBOX").await.unwrap();

        // Insert a message
        let msg = ImapMessage {
            uid: 1,
            message_id: None,
            subject: Some("Hello".into()),
            from: None,
            to: None,
            cc: None,
            date: None,
            in_reply_to: None,
            references: None,
            flags: vec![],
            size: None,
            body: None,
            body_text: None,
            body_html: None,
            has_attachments: false,
        };
        upsert_message(&pool, &account_id, &folder_id, &msg)
            .await
            .unwrap();

        // Save cursor
        save_cursor(&pool, &account_id, &folder_id, "imap", 100, 1)
            .await
            .unwrap();

        // Clear
        clear_folder_messages(&pool, &folder_id).await.unwrap();

        let msg_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM message WHERE folder_id = ?")
            .bind(&folder_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(msg_count, 0);

        let cursor_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM sync_cursor WHERE folder_id = ?")
                .bind(&folder_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(cursor_count, 0);
    }

    #[tokio::test]
    async fn update_folder_counts_works() {
        let pool = test_pool().await;
        let (_, account_id) = seed_user_and_account(&pool).await;

        upsert_folder(&pool, &account_id, "INBOX", Some("/"))
            .await
            .unwrap();
        let folder_id = get_folder_id(&pool, &account_id, "INBOX").await.unwrap();

        // Insert 3 messages: 2 read, 1 unread
        for i in 1..=3u32 {
            let msg = ImapMessage {
                uid: i,
                message_id: None,
                subject: Some(format!("Msg {i}")),
                from: None,
                to: None,
                cc: None,
                date: None,
                in_reply_to: None,
                references: None,
                flags: if i <= 2 {
                    vec!["\\Seen".into()]
                } else {
                    vec![]
                },
                size: None,
                body: None,
                body_text: None,
                body_html: None,
                has_attachments: false,
            };
            upsert_message(&pool, &account_id, &folder_id, &msg)
                .await
                .unwrap();
        }

        update_folder_counts(&pool, &folder_id).await.unwrap();

        let total: i32 = sqlx::query_scalar("SELECT total_messages FROM folder WHERE id = ?")
            .bind(&folder_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(total, 3);

        let unread: i32 = sqlx::query_scalar("SELECT unread_messages FROM folder WHERE id = ?")
            .bind(&folder_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(unread, 1);
    }
}
