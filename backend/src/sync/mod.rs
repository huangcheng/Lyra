//! Sync engine module.
//!
//! Orchestrates IMAP adapters, writes to storage, and tracks sync state
//! via the `sync_cursor` table for idempotent, resumable sync.
//!
//! See `docs/specs/2026-08-20-lyra-sync-and-protocols-spec.md`.

#![allow(clippy::doc_markdown)]

mod http;
mod imap_loop;
pub(crate) mod jmap_client;
mod jmap_loop;
mod recovery;
mod send;
mod store;
mod types;

pub use http::routes;
pub use types::{SyncError, SyncResponse};

pub(crate) use imap_loop::imap_sync_account;
pub(crate) use jmap_loop::jmap_sync_account;
pub(crate) use send::{
    MAX_ATTACHMENTS_PER_SEND, deliver_jmap, deliver_smtp, parse_address_list, prepare_jmap_send,
    prepare_smtp_send,
};

#[cfg(test)]
pub(crate) use send::{outbound_from_raw, resolve_send_plugin};
#[cfg(test)]
pub(crate) use store::{
    clear_folder_messages, clear_jmap_cursor, get_folder_id, imap_folder_depth,
    imap_folder_display_name, infer_folder_role, load_cursor, load_jmap_cursor, parse_cursor_value,
    persist_imap_folder_batch, save_cursor, save_cursor_in_tx, save_jmap_cursor, special_use_role,
    split_imap_folder_path, update_folder_counts, upsert_folder, upsert_message,
    upsert_message_in_tx,
};

#[cfg(test)]
pub(crate) use http::{
    query_user_messages, sqlite_utc_datetime, sync_event_json, trigger_sync,
    user_has_active_sync_job,
};

use sea_orm::sea_query::{Expr, Query};
use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QuerySelect, Value};

use crate::db_row::{IdParam, id_param};
use crate::entities::mail_account as account_entity;
use crate::kernel::App;
use crate::protocol::SyncCtx;
use crate::storage::DbPool;

// ── Sync orchestration ──────────────────────────────────────────────

/// Run a full sync for a mail account.
///
/// Loads the account, resolves the receive plugin from `receive_protocol`
/// (legacy `protocol` if empty), and dispatches to `plugin.sync_account`.
pub async fn run_account_sync(
    db: &DbPool,
    app: &App,
    user_id: &str,
    account_id: &str,
) -> Result<SyncResponse, SyncError> {
    let id_value = bind_id(db, account_id)?;
    let user_value = bind_id(db, user_id)?;
    let row = account_entity::Entity::find()
        .select_only()
        .column(account_entity::Column::ReceiveProtocol)
        .column(account_entity::Column::Protocol)
        .column(account_entity::Column::IsActive)
        .column(account_entity::Column::SyncEnabled)
        .filter(
            sea_orm::sea_query::Condition::all()
                .add(account_entity::Column::Id.eq(id_value.clone()))
                .add(account_entity::Column::UserId.eq(user_value)),
        )
        .into_tuple::<(Option<String>, String, bool, bool)>()
        .one(&db.orm())
        .await
        .map_err(|e| SyncError::Database(unpack_db_err(e)))?
        .ok_or(SyncError::AccountNotFound)?;

    let (receive_protocol, protocol, is_active, sync_enabled) = row;

    if !is_active || !sync_enabled {
        return Err(SyncError::AccountDisabled);
    }

    let receive_id = receive_protocol
        .filter(|s| !s.is_empty())
        .unwrap_or(protocol);

    let plugin = app
        .receive(&receive_id)
        .map_err(|e| SyncError::InvalidInput(e.to_string()))?;
    let outcome = plugin
        .sync_account(&SyncCtx {
            account_id: account_id.to_string(),
            user_id: user_id.to_string(),
        })
        .await
        .map_err(SyncError::Protocol)?;

    let stmt = Query::update()
        .table(account_entity::Entity)
        .value(
            account_entity::Column::LastSyncAt,
            Expr::current_timestamp(),
        )
        .value(account_entity::Column::UpdatedAt, Expr::current_timestamp())
        .and_where(account_entity::Column::Id.eq(bind_id(db, account_id)?))
        .to_owned();
    db.orm()
        .execute(&stmt)
        .await
        .map_err(|e| SyncError::Database(unpack_db_err(e)))?;

    Ok(SyncResponse {
        account_id: account_id.to_string(),
        status: "completed".into(),
        folders_synced: usize::try_from(outcome.folders_synced).unwrap_or(usize::MAX),
        messages_synced: usize::try_from(outcome.messages_synced).unwrap_or(usize::MAX),
        messages_updated: 0,
        messages_deleted: 0,
    })
}

// ── Tests ───────────────────────────────────────────────────────────

/// Unwrap sea_orm's Arc-wrapped driver error into the SyncError seam.
fn unpack_db_err(err: sea_orm::DbErr) -> sqlx::Error {
    use sea_orm::RuntimeErr;
    match err {
        sea_orm::DbErr::Exec(RuntimeErr::SqlxError(e))
        | sea_orm::DbErr::Query(RuntimeErr::SqlxError(e))
        | sea_orm::DbErr::Conn(RuntimeErr::SqlxError(e)) => std::sync::Arc::try_unwrap(e)
            .unwrap_or_else(|shared| sqlx::Error::Protocol(shared.to_string())),
        other => sqlx::Error::Protocol(other.to_string()),
    }
}

/// Dialect-aware id bind for sea_query filters (TEXT on SQLite / UUID on PG).
fn bind_id(db: &DbPool, id: &str) -> Result<Value, SyncError> {
    Ok(match id_param(db, id)? {
        IdParam::Text(s) => Value::String(Some(s)),
        IdParam::Uuid(u) => Value::Uuid(Some(u)),
    })
}
#[cfg(test)]
mod tests {
    use super::store::imap_message_external_id;
    use super::*;
    use crate::auth::{AuthState, AuthUser};
    use crate::imap::ImapMessage;
    use crate::kernel::{App, AppEvent};
    use crate::storage::{DbPool, Storage};
    use axum::{
        Json,
        extract::{Path, State},
        http::StatusCode,
        response::IntoResponse,
    };
    use uuid::Uuid;

    fn as_db(pool: &sqlx::SqlitePool) -> DbPool {
        DbPool::Sqlite(pool.clone())
    }

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

    /// Seed a user (with a wrapped DEK) and account in the test DB,
    /// return `(user_id, account_id)`.
    async fn seed_user_and_account(pool: &sqlx::SqlitePool) -> (String, String) {
        let user_id = Uuid::new_v7(uuid::Timestamp::now(uuid::NoContext)).to_string();
        let account_id = Uuid::new_v7(uuid::Timestamp::now(uuid::NoContext)).to_string();

        // Insert user with a wrapped DEK under the shared test master key
        crate::auth::install_test_master_key();
        let dek = crate::crypto::generate_key();
        let kek = crate::crypto::derive_user_kek(crate::auth::TEST_MASTER_KEY, &user_id);
        let wrapped_dek = crate::crypto::wrap_dek(&kek, &dek).unwrap();
        sqlx::query(
            "INSERT INTO lyra_user (id, username, password_hash, encrypted_dek) VALUES (?, ?, ?, ?)",
        )
        .bind(&user_id)
        .bind(format!("testuser-{user_id}"))
        .bind("hash")
        .bind(&wrapped_dek)
        .execute(pool)
        .await
        .unwrap();

        // Insert account
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
    fn send_rejects_unknown_protocol() {
        // Handler mapping: load send_protocol and call registry → 400 for unknown ids.
        // (Full HTTP spin-up is heavy; App::send("graph") fail-closed is Task 2.)
        let mut app = App::new();
        app.provide("storage");
        for plugin in crate::plugins::builtin_plugins() {
            app.register_plugin(plugin.as_ref()).unwrap();
        }

        let err = resolve_send_plugin(&app, "graph")
            .err()
            .expect("graph must be unknown");
        assert!(
            matches!(&err, SyncError::InvalidInput(msg) if msg.contains("graph")),
            "expected InvalidInput mentioning graph, got {err}"
        );

        let response = err.into_response();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn send_rejects_unknown_protocol_from_account_column() {
        let pool = test_pool().await;
        let (_, account_id) = seed_user_and_account(&pool).await;

        sqlx::query("UPDATE mail_account SET send_protocol = ? WHERE id = ?")
            .bind("graph")
            .bind(&account_id)
            .execute(&pool)
            .await
            .unwrap();

        let send_protocol: String =
            sqlx::query_scalar("SELECT send_protocol FROM mail_account WHERE id = ?")
                .bind(&account_id)
                .fetch_one(&pool)
                .await
                .unwrap();

        let mut app = App::new();
        app.provide("storage");
        for plugin in crate::plugins::builtin_plugins() {
            app.register_plugin(plugin.as_ref()).unwrap();
        }

        let err = resolve_send_plugin(&app, &send_protocol)
            .err()
            .expect("graph must be unknown");
        let response = err.into_response();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn outbound_from_raw_json_preserves_recipients() {
        let raw = r#"{
            "from_email":"ignored@example.com",
            "from_name":null,
            "to":[[null,"alice@example.com"]],
            "cc":[],
            "bcc":[],
            "subject":"Hi",
            "body_text":"Hello",
            "body_html":null,
            "in_reply_to":null,
            "references":null
        }"#;
        let outbound = outbound_from_raw("acct@example.com".into(), raw).unwrap();
        assert_eq!(outbound.from_email, "acct@example.com");
        assert_eq!(outbound.to.len(), 1);
        assert_eq!(outbound.to[0].1, "alice@example.com");
        assert_eq!(outbound.subject, "Hi");
        assert_eq!(outbound.body_text.as_deref(), Some("Hello"));
    }

    #[tokio::test]
    async fn user_has_active_sync_job_filters_by_user() {
        let pool = test_pool().await;
        let (user_id, account_id) = seed_user_and_account(&pool).await;
        let other_user = Uuid::new_v7(uuid::Timestamp::now(uuid::NoContext)).to_string();

        assert!(
            !user_has_active_sync_job(&as_db(&pool), &user_id)
                .await
                .unwrap()
        );

        crate::jobs::enqueue(
            &as_db(&pool),
            &crate::jobs::JobPayload::SyncAccount {
                account_id: account_id.clone(),
                user_id: user_id.clone(),
            },
            &chrono::Utc::now().to_rfc3339(),
        )
        .await
        .unwrap();

        assert!(
            user_has_active_sync_job(&as_db(&pool), &user_id)
                .await
                .unwrap()
        );
        assert!(
            !user_has_active_sync_job(&as_db(&pool), &other_user)
                .await
                .unwrap()
        );
    }

    fn test_auth_state(pool: sqlx::SqlitePool) -> AuthState {
        crate::auth::install_test_master_key();
        let config = crate::config::Config {
            listen_addr: "127.0.0.1:0".into(),
            database_url: "sqlite::memory:".into(),
            data_dir: std::env::temp_dir().to_string_lossy().into_owned(),
            min_password_length: 8,
            sync_max_concurrent: 3,
            sync_poll_secs: 300,
            max_attachment_bytes: 25 * 1024 * 1024,
            redis_url: None,
            master_key: crate::auth::TEST_MASTER_KEY.to_vec(),
            ms_oauth: None,
            yandex_oauth: None,
        };
        AuthState::new(
            DbPool::Sqlite(pool),
            &config,
            std::sync::Arc::new(App::new()),
            std::sync::Arc::new(crate::kv::MemoryKv::new()),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn trigger_sync_returns_existing_job_when_already_queued() {
        let pool = test_pool().await;
        let (user_id, account_id) = seed_user_and_account(&pool).await;
        let state = test_auth_state(pool.clone());

        let (status1, Json(first)) = trigger_sync(
            State(state.clone()),
            Path(account_id.clone()),
            AuthUser(user_id.clone()),
        )
        .await
        .unwrap();
        assert_eq!(status1, StatusCode::ACCEPTED);

        let (status2, Json(second)) =
            trigger_sync(State(state), Path(account_id), AuthUser(user_id))
                .await
                .unwrap();
        // 202 + existing job id (not 409): Settings poller can keep polling.
        assert_eq!(status2, StatusCode::ACCEPTED);
        assert_eq!(first.job_id, second.job_id);

        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM jobs WHERE kind = 'sync_account' AND status IN ('pending', 'running')",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(count, 1, "must not enqueue a second in-flight sync");
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
    fn special_use_role_maps_rfc6154_attributes() {
        // async-imap `Attribute` Debug strings
        assert_eq!(special_use_role(&["Archive".to_string()]), Some("archive"));
        assert_eq!(special_use_role(&["Drafts".to_string()]), Some("drafts"));
        assert_eq!(special_use_role(&["Sent".to_string()]), Some("sent"));
        assert_eq!(special_use_role(&["Trash".to_string()]), Some("trash"));
        assert_eq!(
            special_use_role(&["Custom(\"\\\\Junk\")".to_string()]),
            Some("spam")
        );
        assert_eq!(special_use_role(&["All".to_string()]), Some("archive"));
        // \Flagged is a smart view, not a folder role
        assert_eq!(special_use_role(&["Flagged".to_string()]), None);
        assert_eq!(special_use_role(&["HasNoChildren".to_string()]), None);
        assert_eq!(special_use_role(&[]), None);
    }

    #[tokio::test]
    async fn upsert_folder_special_use_attribute_wins_over_name() {
        let pool = test_pool().await;
        let (_, account_id) = seed_user_and_account(&pool).await;

        // Server-localized name ("Gesendet") + \Sent attribute → role sent
        upsert_folder(
            &as_db(&pool),
            &account_id,
            "Gesendet",
            Some("/"),
            &["Sent".to_string(), "HasNoChildren".to_string()],
        )
        .await
        .unwrap();

        let row: (Option<String>,) =
            sqlx::query_as("SELECT role FROM folder WHERE account_id = ? AND external_id = ?")
                .bind(&account_id)
                .bind("Gesendet")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(row.0.as_deref(), Some("sent"));

        // Re-upsert without attributes falls back to name inference and clears the role
        upsert_folder(&as_db(&pool), &account_id, "Gesendet", Some("/"), &[])
            .await
            .unwrap();
        let row: (Option<String>,) =
            sqlx::query_as("SELECT role FROM folder WHERE account_id = ? AND external_id = ?")
                .bind(&account_id)
                .bind("Gesendet")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(row.0, None);
    }

    #[test]
    fn split_imap_folder_path_top_level() {
        let (parent, leaf) = split_imap_folder_path("INBOX", Some("/"));
        assert_eq!(parent, None);
        assert_eq!(leaf, "INBOX");
    }

    #[test]
    fn split_imap_folder_path_nested() {
        let (parent, leaf) = split_imap_folder_path("Archive/Projects/Lyra", Some("/"));
        assert_eq!(parent, Some("Archive/Projects"));
        assert_eq!(leaf, "Lyra");
    }

    #[test]
    fn imap_folder_display_name_uses_leaf_only() {
        assert_eq!(
            imap_folder_display_name("Archive/&Xi5SqWUvYwE-", Some("/")),
            crate::imap::decode_imap_mailbox_name("&Xi5SqWUvYwE-")
        );
    }

    #[test]
    fn imap_folder_depth_counts_delimiters() {
        assert_eq!(imap_folder_depth("INBOX", Some("/")), 0);
        assert_eq!(imap_folder_depth("Archive/Projects", Some("/")), 1);
        assert_eq!(imap_folder_depth("Archive/A/B", Some("/")), 2);
    }

    #[test]
    fn parse_cursor_value_valid() {
        let cursor = parse_cursor_value("12345:678");
        assert_eq!(cursor.uid_validity, 12345);
        assert_eq!(cursor.last_uid, 678);
    }

    #[test]
    fn parse_cursor_value_with_modseq() {
        let cursor = parse_cursor_value("12345:678:999");
        assert_eq!(cursor.uid_validity, 12345);
        assert_eq!(cursor.last_uid, 678);
        assert_eq!(cursor.last_modseq, 999);
    }

    #[test]
    fn parse_cursor_value_invalid() {
        let cursor = parse_cursor_value("garbage");
        assert_eq!(cursor.uid_validity, 0);
        assert_eq!(cursor.last_uid, 0);
        assert_eq!(cursor.last_modseq, 0);
    }

    #[tokio::test]
    async fn upsert_folder_insert_and_update() {
        let pool = test_pool().await;
        let (_, account_id) = seed_user_and_account(&pool).await;

        // First insert
        upsert_folder(&as_db(&pool), &account_id, "INBOX", Some("/"), &[])
            .await
            .unwrap();

        let id1 = get_folder_id(&as_db(&pool), &account_id, "INBOX")
            .await
            .unwrap();

        // Second upsert (update) — should not create a new row
        upsert_folder(&as_db(&pool), &account_id, "INBOX", Some("/"), &[])
            .await
            .unwrap();

        let id2 = get_folder_id(&as_db(&pool), &account_id, "INBOX")
            .await
            .unwrap();

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
    async fn upsert_folder_decodes_modified_utf7_display_name() {
        let pool = test_pool().await;
        let (_, account_id) = seed_user_and_account(&pool).await;
        let wire = "Archive/&Xi5SqWUvYwE-";

        upsert_folder(&as_db(&pool), &account_id, wire, Some("/"), &[])
            .await
            .unwrap();

        let row: (String, String) = sqlx::query_as(
            "SELECT external_id, name FROM folder WHERE account_id = ? AND external_id = ?",
        )
        .bind(&account_id)
        .bind(wire)
        .fetch_one(&pool)
        .await
        .unwrap();

        assert_eq!(
            row.0, wire,
            "external_id must stay wire-encoded for IMAP SELECT"
        );
        assert!(
            !row.1.contains("&Xi5"),
            "display name should be decoded, got: {}",
            row.1
        );
        assert!(
            row.1.contains('档') || !row.1.contains('/'),
            "expected decoded leaf folder name, got: {}",
            row.1
        );

        // Re-upsert updates display name without creating a duplicate row
        upsert_folder(&as_db(&pool), &account_id, wire, Some("/"), &[])
            .await
            .unwrap();

        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM folder WHERE account_id = ?")
            .bind(&account_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn upsert_folder_links_imap_parent_id() {
        let pool = test_pool().await;
        let (_, account_id) = seed_user_and_account(&pool).await;

        upsert_folder(&as_db(&pool), &account_id, "Archive", Some("/"), &[])
            .await
            .unwrap();
        upsert_folder(
            &as_db(&pool),
            &account_id,
            "Archive/Projects",
            Some("/"),
            &[],
        )
        .await
        .unwrap();

        let archive_id = get_folder_id(&as_db(&pool), &account_id, "Archive")
            .await
            .unwrap();
        let row: (Option<String>, String) = sqlx::query_as(
            "SELECT parent_id, name FROM folder WHERE account_id = ? AND external_id = ?",
        )
        .bind(&account_id)
        .bind("Archive/Projects")
        .fetch_one(&pool)
        .await
        .unwrap();

        assert_eq!(row.1, "Projects");
        assert_eq!(row.0.as_deref(), Some(archive_id.as_str()));
    }

    #[tokio::test]
    async fn upsert_message_idempotent() {
        let pool = test_pool().await;
        let (_, account_id) = seed_user_and_account(&pool).await;

        // Create a folder first
        upsert_folder(&as_db(&pool), &account_id, "INBOX", Some("/"), &[])
            .await
            .unwrap();
        let folder_id = get_folder_id(&as_db(&pool), &account_id, "INBOX")
            .await
            .unwrap();

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
            attachments: vec![],
        };

        // First insert
        let was_new = upsert_message(&as_db(&pool), &account_id, &folder_id, &msg)
            .await
            .unwrap();
        assert!(was_new, "first insert should return true");

        // Second upsert (update flags)
        let mut msg2 = msg.clone();
        msg2.flags = vec!["\\Seen".into(), "\\Flagged".into()];
        let was_new2 = upsert_message(&as_db(&pool), &account_id, &folder_id, &msg2)
            .await
            .unwrap();
        assert!(!was_new2, "second upsert should return false (update)");

        // Verify only one row
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM message WHERE account_id = ? AND external_id = ?",
        )
        .bind(&account_id)
        .bind(imap_message_external_id(&folder_id, 42))
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(count, 1, "should have exactly one message row");

        // Verify flags were updated
        let flags: String = sqlx::query_scalar(
            "SELECT flags FROM message WHERE account_id = ? AND external_id = ?",
        )
        .bind(&account_id)
        .bind(imap_message_external_id(&folder_id, 42))
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(flags.contains("Flagged"), "flags should be updated");
    }

    #[tokio::test]
    async fn upsert_message_fills_empty_envelope_on_resync() {
        let pool = test_pool().await;
        let (_, account_id) = seed_user_and_account(&pool).await;
        upsert_folder(&as_db(&pool), &account_id, "INBOX", Some("/"), &[])
            .await
            .unwrap();
        let folder_id = get_folder_id(&as_db(&pool), &account_id, "INBOX")
            .await
            .unwrap();

        let blank = ImapMessage {
            uid: 7,
            message_id: None,
            subject: None,
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
            attachments: vec![],
        };
        upsert_message(&as_db(&pool), &account_id, &folder_id, &blank)
            .await
            .unwrap();

        let filled = ImapMessage {
            subject: Some("Welcome".into()),
            from: Some("hello@example.com".into()),
            to: Some("you@example.com".into()),
            date: Some("2026-08-23T10:00:00Z".into()),
            flags: vec!["\\Seen".into()],
            ..blank
        };
        let was_new = upsert_message(&as_db(&pool), &account_id, &folder_id, &filled)
            .await
            .unwrap();
        assert!(!was_new);

        let row: (Option<String>, Option<String>, Option<String>) = sqlx::query_as(
            "SELECT subject, from_address, snippet FROM message \
             WHERE account_id = ? AND external_id = ?",
        )
        .bind(&account_id)
        .bind(imap_message_external_id(&folder_id, 7))
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(row.0.as_deref(), Some("Welcome"));
        assert!(
            row.1
                .as_deref()
                .is_some_and(|f| f.contains("hello@example.com")),
            "from_address={:?}",
            row.1
        );
        assert_eq!(row.2.as_deref(), Some("Welcome"));
    }

    #[tokio::test]
    async fn folder_sync_batch_commits_messages_cursor_and_counts() {
        let pool = test_pool().await;
        let (_, account_id) = seed_user_and_account(&pool).await;
        upsert_folder(&as_db(&pool), &account_id, "INBOX", Some("/"), &[])
            .await
            .unwrap();
        let folder_id = get_folder_id(&as_db(&pool), &account_id, "INBOX")
            .await
            .unwrap();

        let (new, updated) = persist_imap_folder_batch(
            &as_db(&pool),
            &account_id,
            &folder_id,
            &[sample_imap_message(1), sample_imap_message(2)],
            99,
            2,
            0,
            false,
        )
        .await
        .unwrap();
        assert_eq!(new, 2);
        assert_eq!(updated, 0);

        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM message WHERE account_id = ?")
            .bind(&account_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 2);

        let cursor = load_cursor(&as_db(&pool), &account_id, &folder_id)
            .await
            .unwrap()
            .expect("cursor");
        assert_eq!(cursor.uid_validity, 99);
        assert_eq!(cursor.last_uid, 2);

        let total: i64 = sqlx::query_scalar("SELECT total_messages FROM folder WHERE id = ?")
            .bind(&folder_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(total, 2);
    }

    #[tokio::test]
    async fn folder_sync_transaction_rolls_back_messages_and_cursor() {
        let pool = test_pool().await;
        let (_, account_id) = seed_user_and_account(&pool).await;
        upsert_folder(&as_db(&pool), &account_id, "INBOX", Some("/"), &[])
            .await
            .unwrap();
        let folder_id = get_folder_id(&as_db(&pool), &account_id, "INBOX")
            .await
            .unwrap();

        let db = as_db(&pool);
        let mut tx = db.begin().await.unwrap();
        upsert_message_in_tx(
            &mut tx,
            &db,
            &account_id,
            &folder_id,
            &sample_imap_message(7),
        )
        .await
        .unwrap();
        save_cursor_in_tx(&mut tx, &db, &account_id, &folder_id, "imap", 1, 7, 0)
            .await
            .unwrap();
        tx.rollback().await.unwrap();

        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM message WHERE account_id = ?")
            .bind(&account_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 0, "rolled-back upsert must not be visible");
        let cursor = load_cursor(&db, &account_id, &folder_id).await.unwrap();
        assert!(cursor.is_none(), "rolled-back cursor must not be visible");
    }

    #[tokio::test]
    async fn save_and_load_cursor() {
        let pool = test_pool().await;
        let (_, account_id) = seed_user_and_account(&pool).await;

        upsert_folder(&as_db(&pool), &account_id, "INBOX", Some("/"), &[])
            .await
            .unwrap();
        let folder_id = get_folder_id(&as_db(&pool), &account_id, "INBOX")
            .await
            .unwrap();

        // Save cursor
        save_cursor(
            &as_db(&pool),
            &account_id,
            &folder_id,
            "imap",
            12345,
            100,
            0,
        )
        .await
        .unwrap();

        // Load cursor
        let cursor = load_cursor(&as_db(&pool), &account_id, &folder_id)
            .await
            .unwrap()
            .expect("cursor should exist");

        assert_eq!(cursor.uid_validity, 12345);
        assert_eq!(cursor.last_uid, 100);

        // Update cursor (idempotent upsert)
        save_cursor(
            &as_db(&pool),
            &account_id,
            &folder_id,
            "imap",
            12345,
            200,
            0,
        )
        .await
        .unwrap();

        let cursor2 = load_cursor(&as_db(&pool), &account_id, &folder_id)
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
    async fn jmap_state_cursor_roundtrip() {
        let pool = test_pool().await;
        let (_, account_id) = seed_user_and_account(&pool).await;

        upsert_folder(&as_db(&pool), &account_id, "INBOX", Some("/"), &[])
            .await
            .unwrap();
        let folder_id = get_folder_id(&as_db(&pool), &account_id, "INBOX")
            .await
            .unwrap();

        // A JMAP queryState is an opaque server token — it must round-trip verbatim,
        // because it is sent back to the server as `sinceQueryState` on the next sync.
        let query_state = "jmap-state-abc123";

        save_jmap_cursor(&as_db(&pool), &account_id, &folder_id, query_state)
            .await
            .unwrap();

        // The raw token must be stored as-is, not a hash of it.
        let raw: String = sqlx::query_scalar(
            "SELECT cursor_value FROM sync_cursor \
             WHERE account_id = ? AND folder_id = ? AND cursor_type = 'state_token'",
        )
        .bind(&account_id)
        .bind(&folder_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            raw, query_state,
            "stored cursor must be the raw queryState token"
        );

        // What we send back as sinceQueryState must equal the original token.
        let since_state = load_jmap_cursor(&as_db(&pool), &account_id, &folder_id)
            .await
            .unwrap();
        assert_eq!(since_state.as_deref(), Some(query_state));

        // Idempotent upsert: saving a newer state replaces the old one.
        save_jmap_cursor(&as_db(&pool), &account_id, &folder_id, "jmap-state-def456")
            .await
            .unwrap();
        let since_state = load_jmap_cursor(&as_db(&pool), &account_id, &folder_id)
            .await
            .unwrap();
        assert_eq!(since_state.as_deref(), Some("jmap-state-def456"));

        // Only one cursor row per folder.
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM sync_cursor WHERE account_id = ?")
                .bind(&account_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(count, 1);

        clear_jmap_cursor(&as_db(&pool), &account_id, &folder_id)
            .await
            .unwrap();
        let since_state = load_jmap_cursor(&as_db(&pool), &account_id, &folder_id)
            .await
            .unwrap();
        assert!(since_state.is_none());
    }

    #[test]
    fn sync_event_json_matches_spec_type_names() {
        let started = sync_event_json(&AppEvent::SyncStarted {
            account_id: "acc".into(),
        });
        assert_eq!(started["type"], "sync_started");
        assert_eq!(started["accountId"], "acc");
        let err = sync_event_json(&AppEvent::SyncError {
            account_id: "acc".into(),
            error: "IMAP error".into(),
        });
        assert_eq!(err["type"], "sync_error");
        assert_eq!(err["error"], "IMAP error");
    }

    #[tokio::test]
    async fn clear_folder_messages_works() {
        let pool = test_pool().await;
        let (_, account_id) = seed_user_and_account(&pool).await;

        upsert_folder(&as_db(&pool), &account_id, "INBOX", Some("/"), &[])
            .await
            .unwrap();
        let folder_id = get_folder_id(&as_db(&pool), &account_id, "INBOX")
            .await
            .unwrap();

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
            attachments: vec![],
        };
        upsert_message(&as_db(&pool), &account_id, &folder_id, &msg)
            .await
            .unwrap();

        // Save cursor (IMAP folder cursor) and an account-level JMAP
        // `email_state` row anchored on the same folder.
        save_cursor(&as_db(&pool), &account_id, &folder_id, "imap", 100, 1, 0)
            .await
            .unwrap();
        super::store::save_jmap_email_state(&as_db(&pool), &account_id, &folder_id, "es-1")
            .await
            .unwrap();

        // Clear
        clear_folder_messages(&as_db(&pool), &folder_id)
            .await
            .unwrap();

        let msg_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM message WHERE folder_id = ?")
            .bind(&folder_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(msg_count, 0);

        // Folder-scoped cursors are wiped with the messages…
        let cursor_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sync_cursor WHERE folder_id = ? AND cursor_type != 'email_state'",
        )
        .bind(&folder_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(cursor_count, 0);
        // …but the account-level JMAP `email_state` anchored here survives:
        // an IMAP-path wipe must not disable the Email/changes safety net.
        assert_eq!(
            super::store::load_jmap_email_state(&as_db(&pool), &account_id, &folder_id)
                .await
                .unwrap()
                .as_deref(),
            Some("es-1")
        );
    }

    #[tokio::test]
    async fn update_folder_counts_works() {
        let pool = test_pool().await;
        let (_, account_id) = seed_user_and_account(&pool).await;

        upsert_folder(&as_db(&pool), &account_id, "INBOX", Some("/"), &[])
            .await
            .unwrap();
        let folder_id = get_folder_id(&as_db(&pool), &account_id, "INBOX")
            .await
            .unwrap();

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
                attachments: vec![],
            };
            upsert_message(&as_db(&pool), &account_id, &folder_id, &msg)
                .await
                .unwrap();
        }

        update_folder_counts(&as_db(&pool), &folder_id)
            .await
            .unwrap();

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

    fn sample_imap_message(uid: u32) -> ImapMessage {
        ImapMessage {
            uid,
            message_id: Some(format!("<snooze{uid}@example.com>")),
            subject: Some(format!("Snooze {uid}")),
            from: Some("sender@example.com".into()),
            to: Some("test@example.com".into()),
            cc: None,
            date: Some("2026-08-22T10:00:00Z".into()),
            in_reply_to: None,
            references: None,
            flags: vec![],
            size: None,
            body: None,
            body_text: None,
            body_html: None,
            has_attachments: false,
            attachments: vec![],
        }
    }

    async fn message_id_for_uid(
        pool: &sqlx::SqlitePool,
        account_id: &str,
        folder_id: &str,
        uid: u32,
    ) -> String {
        sqlx::query_scalar("SELECT id FROM message WHERE account_id = ? AND external_id = ?")
            .bind(account_id)
            .bind(imap_message_external_id(folder_id, uid))
            .fetch_one(pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn inbox_hides_snoozed_message() {
        let pool = test_pool().await;
        let (user_id, account_id) = seed_user_and_account(&pool).await;
        upsert_folder(&as_db(&pool), &account_id, "INBOX", Some("/"), &[])
            .await
            .unwrap();
        let folder_id = get_folder_id(&as_db(&pool), &account_id, "INBOX")
            .await
            .unwrap();
        upsert_message(
            &as_db(&pool),
            &account_id,
            &folder_id,
            &sample_imap_message(7),
        )
        .await
        .unwrap();
        let message_id = message_id_for_uid(&pool, &account_id, &folder_id, 7).await;
        let future = sqlite_utc_datetime(chrono::Utc::now() + chrono::Duration::hours(1));
        sqlx::query("UPDATE message SET snoozed_until = ? WHERE id = ?")
            .bind(&future)
            .bind(&message_id)
            .execute(&pool)
            .await
            .unwrap();

        let inbox = query_user_messages(&as_db(&pool), &user_id, Some("inbox"), None)
            .await
            .unwrap();
        assert!(
            inbox.is_empty(),
            "future snoozed_until must hide the row from inbox"
        );
    }

    #[tokio::test]
    async fn inbox_shows_overdue_same_day_snooze() {
        let pool = test_pool().await;
        let (user_id, account_id) = seed_user_and_account(&pool).await;
        upsert_folder(&as_db(&pool), &account_id, "INBOX", Some("/"), &[])
            .await
            .unwrap();
        let folder_id = get_folder_id(&as_db(&pool), &account_id, "INBOX")
            .await
            .unwrap();
        upsert_message(
            &as_db(&pool),
            &account_id,
            &folder_id,
            &sample_imap_message(10),
        )
        .await
        .unwrap();
        let message_id = message_id_for_uid(&pool, &account_id, &folder_id, 10).await;

        // Client sends RFC3339 with `T`; store the SQLite-safe form the handler writes.
        let past_rfc = (chrono::Utc::now() - chrono::Duration::seconds(30)).to_rfc3339();
        assert!(
            past_rfc.contains('T'),
            "regression requires RFC3339 T separator"
        );
        let past_sql = sqlite_utc_datetime(
            chrono::DateTime::parse_from_rfc3339(&past_rfc)
                .unwrap()
                .to_utc(),
        );
        assert!(
            !past_sql.contains('T'),
            "stored snoozed_until must use space, not T"
        );

        sqlx::query("UPDATE message SET snoozed_until = ? WHERE id = ?")
            .bind(&past_sql)
            .bind(&message_id)
            .execute(&pool)
            .await
            .unwrap();

        let inbox = query_user_messages(&as_db(&pool), &user_id, Some("inbox"), None)
            .await
            .unwrap();
        assert_eq!(
            inbox.len(),
            1,
            "same-day overdue snooze must be visible in inbox"
        );
        assert_eq!(inbox[0].id, message_id);
    }

    #[tokio::test]
    async fn repair_heals_mojibake_subject_and_clears_body() {
        let pool = test_pool().await;
        let (_user_id, account_id) = seed_user_and_account(&pool).await;
        upsert_folder(&as_db(&pool), &account_id, "INBOX", Some("/"), &[])
            .await
            .unwrap();
        let folder_id = get_folder_id(&as_db(&pool), &account_id, "INBOX")
            .await
            .unwrap();

        // Row as written by the charset-blind parser: U+FFFD mojibake baked
        // into subject (and snippet), the sender display name, and the body.
        let mut mangled = sample_imap_message(42);
        mangled.subject = Some("\u{FFFD}\u{FFFD}ҵ\u{FFFD}п".into());
        mangled.from = Some("\u{FFFD}\u{FFFD}ҵ\u{FFFD}п <cib@example.com>".into());
        mangled.body_text = Some("\u{FFFD}\u{FFFD}ÿ˵".into());
        upsert_message(&as_db(&pool), &account_id, &folder_id, &mangled)
            .await
            .unwrap();
        // Second row whose subject already healed but whose sender name is
        // still mangled — the scan must catch address-only mojibake too.
        let mut addr_only = sample_imap_message(43);
        addr_only.from = Some("\u{FFFD}\u{FFFD}ͨ\u{FFFD}п <bocom@example.com>".into());
        upsert_message(&as_db(&pool), &account_id, &folder_id, &addr_only)
            .await
            .unwrap();

        // The self-heal pass discovers both rows by U+FFFD.
        let mut uids = store::mojibake_message_uids(&as_db(&pool), &folder_id, 200)
            .await
            .unwrap();
        uids.sort_unstable();
        assert_eq!(uids, vec![42, 43]);

        // Re-fetched envelope with the correctly decoded subject and sender.
        let mut fresh = sample_imap_message(42);
        fresh.subject = Some("兴业银行信用卡账单".into());
        fresh.from = Some("兴业银行 <cib@example.com>".into());
        // Repairing uid 42 leaves the address-only uid 43 in the scan.
        let repaired =
            store::repair_imap_messages(&as_db(&pool), &account_id, &folder_id, &[fresh])
                .await
                .unwrap();
        assert_eq!(repaired, 1);

        let (subject, snippet, from_address, body_text): (
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
        ) = sqlx::query_as(
            "SELECT subject, snippet, from_address, body_text FROM message \
                 WHERE account_id = ? AND external_id = ?",
        )
        .bind(&account_id)
        .bind(imap_message_external_id(&folder_id, 42))
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(subject.as_deref(), Some("兴业银行信用卡账单"));
        assert_eq!(snippet.as_deref(), Some("兴业银行信用卡账单"));
        assert_eq!(
            from_address.as_deref(),
            Some(r#"{"raw":"兴业银行 <cib@example.com>"}"#),
            "mojibake sender display name must heal on re-sync"
        );
        assert_eq!(
            body_text, None,
            "mojibake body must be cleared for lazy re-fetch"
        );

        // The healed row drops out of the repair scan; the still-mangled
        // address-only row remains for the next pass.
        let uids = store::mojibake_message_uids(&as_db(&pool), &folder_id, 200)
            .await
            .unwrap();
        assert_eq!(uids, vec![43]);
    }

    #[tokio::test]
    async fn unsnooze_job_clears_column() {
        let pool = test_pool().await;
        let (user_id, account_id) = seed_user_and_account(&pool).await;
        upsert_folder(&as_db(&pool), &account_id, "INBOX", Some("/"), &[])
            .await
            .unwrap();
        let folder_id = get_folder_id(&as_db(&pool), &account_id, "INBOX")
            .await
            .unwrap();
        upsert_message(
            &as_db(&pool),
            &account_id,
            &folder_id,
            &sample_imap_message(8),
        )
        .await
        .unwrap();
        let message_id = message_id_for_uid(&pool, &account_id, &folder_id, 8).await;
        sqlx::query("UPDATE message SET snoozed_until = ? WHERE id = ?")
            .bind(sqlite_utc_datetime(
                chrono::Utc::now() + chrono::Duration::hours(1),
            ))
            .bind(&message_id)
            .execute(&pool)
            .await
            .unwrap();

        let now = chrono::Utc::now().to_rfc3339();
        let payload = crate::jobs::JobPayload::UnsnoozeMessage {
            message_id: message_id.clone(),
        };
        crate::jobs::enqueue(&as_db(&pool), &payload, &now)
            .await
            .unwrap();
        let claimed = crate::jobs::claim_due(&as_db(&pool), &now)
            .await
            .unwrap()
            .expect("due unsnooze job");

        let app = crate::kernel::App::new();
        let inflight = crate::jobs::InFlight::new();
        let sem = std::sync::Arc::new(tokio::sync::Semaphore::new(1));
        let permit = std::sync::Arc::clone(&sem)
            .try_acquire_owned()
            .expect("test semaphore has a permit");
        crate::jobs::process_job(&as_db(&pool), &app, &inflight, permit, claimed)
            .await
            .expect("unsnooze dispatch must not panic");
        drop(app);

        let snoozed: Option<String> =
            sqlx::query_scalar("SELECT snoozed_until FROM message WHERE id = ?")
                .bind(&message_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(snoozed.is_none(), "dispatch must SET snoozed_until = NULL");

        let inbox = query_user_messages(&as_db(&pool), &user_id, Some("inbox"), None)
            .await
            .unwrap();
        assert_eq!(inbox.len(), 1, "unsnoozed message must reappear in inbox");
        assert_eq!(inbox[0].id, message_id);
    }

    #[tokio::test]
    async fn sync_does_not_clear_snooze() {
        let pool = test_pool().await;
        let (_, account_id) = seed_user_and_account(&pool).await;
        upsert_folder(&as_db(&pool), &account_id, "INBOX", Some("/"), &[])
            .await
            .unwrap();
        let folder_id = get_folder_id(&as_db(&pool), &account_id, "INBOX")
            .await
            .unwrap();
        let msg = sample_imap_message(9);
        upsert_message(&as_db(&pool), &account_id, &folder_id, &msg)
            .await
            .unwrap();
        let message_id = message_id_for_uid(&pool, &account_id, &folder_id, 9).await;
        let until = sqlite_utc_datetime(chrono::Utc::now() + chrono::Duration::days(30));
        sqlx::query("UPDATE message SET snoozed_until = ? WHERE id = ?")
            .bind(&until)
            .bind(&message_id)
            .execute(&pool)
            .await
            .unwrap();

        let mut again = msg.clone();
        again.flags = vec!["\\Seen".into()];
        let was_new = upsert_message(&as_db(&pool), &account_id, &folder_id, &again)
            .await
            .unwrap();
        assert!(!was_new);

        let snoozed: Option<String> =
            sqlx::query_scalar("SELECT snoozed_until FROM message WHERE id = ?")
                .bind(&message_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(
            snoozed.as_deref(),
            Some(until.as_str()),
            "upsert_message must not overwrite snoozed_until"
        );
    }

    #[tokio::test]
    async fn internal_sync_errors_are_masked() {
        // SQL detail must not reach the client.
        let err = SyncError::Database(sqlx::Error::Protocol("no such column: secret_token".into()));
        let res = err.into_response();
        assert_eq!(res.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body = axum::body::to_bytes(res.into_body(), 4096).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["error"], "internal error");
        assert_eq!(json["code"], "internal_error");

        // Upstream protocol chatter (hostnames, usernames) masked as 502.
        let err = SyncError::Protocol("NO auth failed for bob@imap.example.com".into());
        let res = err.into_response();
        assert_eq!(res.status(), StatusCode::BAD_GATEWAY);
        let body = axum::body::to_bytes(res.into_body(), 4096).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["error"], "internal error");
        assert_eq!(json["code"], "bad_gateway");
    }

    #[tokio::test]
    async fn client_sync_errors_stay_descriptive() {
        // 4xx variants are deliberate API surface.
        let err = SyncError::InvalidInput("until must be RFC3339".into());
        let res = err.into_response();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        let body = axum::body::to_bytes(res.into_body(), 4096).await.unwrap();
        let body = std::str::from_utf8(&body).unwrap();
        assert!(body.contains("until must be RFC3339"));

        let res = SyncError::MessageNotFound.into_response();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
    }

    /// Regression: the draft-save lookups must run against an unaliased
    /// `message` table — the shared `m.is_deleted` clause only works in
    /// list queries that alias the table as `m` (broke IMAP draft saves
    /// with "no such column: m.is_deleted").
    #[tokio::test]
    async fn message_id_lookups_skip_soft_deleted_rows() {
        let pool = test_pool().await;
        let (_, account_id) = seed_user_and_account(&pool).await;
        upsert_folder(&as_db(&pool), &account_id, "INBOX", Some("/"), &[])
            .await
            .unwrap();
        let folder_id = get_folder_id(&as_db(&pool), &account_id, "INBOX")
            .await
            .unwrap();

        let msg = ImapMessage {
            uid: 9,
            message_id: Some("<draft-probe@lyra>".into()),
            subject: Some("probe".into()),
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
            attachments: vec![],
        };
        upsert_message(&as_db(&pool), &account_id, &folder_id, &msg)
            .await
            .unwrap();
        let external = imap_message_external_id(&folder_id, 9);
        let db = as_db(&pool);

        let by_header = http::message_id_by_header(&db, &account_id, "<draft-probe@lyra>")
            .await
            .unwrap();
        let by_server = http::message_id_by_server_id(&db, &account_id, &external)
            .await
            .unwrap();
        assert!(by_header.is_some(), "header lookup should find the row");
        assert_eq!(by_header, by_server);

        sqlx::query("UPDATE message SET is_deleted = 1 WHERE account_id = ? AND external_id = ?")
            .bind(&account_id)
            .bind(&external)
            .execute(&pool)
            .await
            .unwrap();

        assert!(
            http::message_id_by_header(&db, &account_id, "<draft-probe@lyra>")
                .await
                .unwrap()
                .is_none(),
            "soft-deleted rows must be invisible"
        );
        assert!(
            http::message_id_by_server_id(&db, &account_id, &external)
                .await
                .unwrap()
                .is_none(),
            "soft-deleted rows must be invisible"
        );
    }

    fn sample_jmap_email(id: &str, thread: Option<&str>) -> super::jmap_client::JmapEmail {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "threadId": thread,
            "subject": format!("Message {id}"),
            "keywords": {},
            "receivedAt": "2026-08-29T10:00:00Z"
        }))
        .unwrap()
    }

    #[tokio::test]
    async fn jmap_upsert_persists_thread_and_folder_moves() {
        let pool = test_pool().await;
        let (_, account_id) = seed_user_and_account(&pool).await;
        upsert_folder(&as_db(&pool), &account_id, "INBOX", Some("/"), &[])
            .await
            .unwrap();
        upsert_folder(&as_db(&pool), &account_id, "Archive", Some("/"), &[])
            .await
            .unwrap();
        let inbox = get_folder_id(&as_db(&pool), &account_id, "INBOX")
            .await
            .unwrap();
        let archive = get_folder_id(&as_db(&pool), &account_id, "Archive")
            .await
            .unwrap();

        super::store::persist_jmap_folder_batch(
            &as_db(&pool),
            &account_id,
            &inbox,
            &[sample_jmap_email("em1", Some("th1"))],
            None,
        )
        .await
        .unwrap();
        let row: (Option<String>, String) = sqlx::query_as(
            "SELECT jmap_thread_id, folder_id FROM message WHERE account_id = ? AND external_id = ?",
        )
        .bind(&account_id)
        .bind("em1")
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(row.0.as_deref(), Some("th1"));
        assert_eq!(row.1, inbox);

        // Re-upsert under another folder = server-side move re-homes the row.
        super::store::persist_jmap_folder_batch(
            &as_db(&pool),
            &account_id,
            &archive,
            &[sample_jmap_email("em1", Some("th1"))],
            None,
        )
        .await
        .unwrap();
        let row: (String,) = sqlx::query_as(
            "SELECT folder_id FROM message WHERE account_id = ? AND external_id = ?",
        )
        .bind(&account_id)
        .bind("em1")
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(row.0, archive);
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM message WHERE account_id = ? AND external_id = ?",
        )
        .bind(&account_id)
        .bind("em1")
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(count, 1, "move must not duplicate the row");
    }

    #[tokio::test]
    async fn jmap_delete_by_external_ids_removes_rows_and_updates_counts() {
        let pool = test_pool().await;
        let (_, account_id) = seed_user_and_account(&pool).await;
        upsert_folder(&as_db(&pool), &account_id, "INBOX", Some("/"), &[])
            .await
            .unwrap();
        let inbox = get_folder_id(&as_db(&pool), &account_id, "INBOX")
            .await
            .unwrap();
        super::store::persist_jmap_folder_batch(
            &as_db(&pool),
            &account_id,
            &inbox,
            &[
                sample_jmap_email("em1", None),
                sample_jmap_email("em2", None),
                sample_jmap_email("em3", None),
            ],
            None,
        )
        .await
        .unwrap();

        let deleted = super::store::delete_jmap_messages_by_external_ids(
            &as_db(&pool),
            &account_id,
            &["em2".to_owned(), "emX".to_owned()],
        )
        .await
        .unwrap();
        assert_eq!(deleted, 1, "unknown ids are skipped");

        let remaining: Vec<String> = sqlx::query_scalar(
            "SELECT external_id FROM message WHERE account_id = ? ORDER BY external_id",
        )
        .bind(&account_id)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(remaining, vec!["em1".to_owned(), "em3".to_owned()]);

        let total: i64 = sqlx::query_scalar("SELECT total_messages FROM folder WHERE id = ?")
            .bind(&inbox)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(total, 2);
    }

    #[tokio::test]
    async fn jmap_email_state_cursor_roundtrip() {
        let pool = test_pool().await;
        let (_, account_id) = seed_user_and_account(&pool).await;
        upsert_folder(&as_db(&pool), &account_id, "INBOX", Some("/"), &[])
            .await
            .unwrap();
        let inbox = get_folder_id(&as_db(&pool), &account_id, "INBOX")
            .await
            .unwrap();

        assert!(
            super::store::load_jmap_email_state(&as_db(&pool), &account_id, &inbox)
                .await
                .unwrap()
                .is_none()
        );
        super::store::save_jmap_email_state(&as_db(&pool), &account_id, &inbox, "email-state-1")
            .await
            .unwrap();
        assert_eq!(
            super::store::load_jmap_email_state(&as_db(&pool), &account_id, &inbox)
                .await
                .unwrap()
                .as_deref(),
            Some("email-state-1")
        );
        super::store::save_jmap_email_state(&as_db(&pool), &account_id, &inbox, "email-state-2")
            .await
            .unwrap();
        assert_eq!(
            super::store::load_jmap_email_state(&as_db(&pool), &account_id, &inbox)
                .await
                .unwrap()
                .as_deref(),
            Some("email-state-2")
        );
        super::store::clear_jmap_email_state(&as_db(&pool), &account_id, &inbox)
            .await
            .unwrap();
        assert!(
            super::store::load_jmap_email_state(&as_db(&pool), &account_id, &inbox)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn folder_id_for_role_finds_inbox() {
        let pool = test_pool().await;
        let (_, account_id) = seed_user_and_account(&pool).await;
        upsert_folder(&as_db(&pool), &account_id, "INBOX", Some("/"), &[])
            .await
            .unwrap();
        let inbox = get_folder_id(&as_db(&pool), &account_id, "INBOX")
            .await
            .unwrap();
        assert_eq!(
            super::store::folder_id_for_role(&as_db(&pool), &account_id, "inbox")
                .await
                .unwrap()
                .as_deref(),
            Some(inbox.as_str())
        );
        assert!(
            super::store::folder_id_for_role(&as_db(&pool), &account_id, "trash")
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn folder_id_for_role_honors_role_override() {
        let pool = test_pool().await;
        let (_, account_id) = seed_user_and_account(&pool).await;
        upsert_folder(&as_db(&pool), &account_id, "INBOX", Some("/"), &[])
            .await
            .unwrap();
        upsert_folder(&as_db(&pool), &account_id, "Archive", Some("/"), &[])
            .await
            .unwrap();
        let archive = get_folder_id(&as_db(&pool), &account_id, "Archive")
            .await
            .unwrap();

        // Base role resolves while no override exists.
        assert_eq!(
            super::store::folder_id_for_role(&as_db(&pool), &account_id, "archive")
                .await
                .unwrap()
                .as_deref(),
            Some(archive.as_str())
        );

        // The override REPLACES the effective role (COALESCE branch): the
        // folder now matches the override and no longer its base role.
        sqlx::query("UPDATE folder SET role_override = 'trash' WHERE id = ?")
            .bind(&archive)
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(
            super::store::folder_id_for_role(&as_db(&pool), &account_id, "trash")
                .await
                .unwrap()
                .as_deref(),
            Some(archive.as_str())
        );
        assert!(
            super::store::folder_id_for_role(&as_db(&pool), &account_id, "archive")
                .await
                .unwrap()
                .is_none(),
            "overridden base role must no longer match"
        );
        // INBOX's own role is unaffected.
        assert!(
            super::store::folder_id_for_role(&as_db(&pool), &account_id, "inbox")
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn first_folder_id_falls_back_to_any_folder() {
        let pool = test_pool().await;
        let (_, account_id) = seed_user_and_account(&pool).await;
        assert!(
            super::store::first_folder_id(&as_db(&pool), &account_id)
                .await
                .unwrap()
                .is_none(),
            "no folders → no anchor"
        );
        upsert_folder(&as_db(&pool), &account_id, "Notes", Some("/"), &[])
            .await
            .unwrap();
        let notes = get_folder_id(&as_db(&pool), &account_id, "Notes")
            .await
            .unwrap();
        assert_eq!(
            super::store::first_folder_id(&as_db(&pool), &account_id)
                .await
                .unwrap()
                .as_deref(),
            Some(notes.as_str()),
            "no inbox-role folder → first folder anchors"
        );
    }

    #[tokio::test]
    async fn jmap_persist_batch_reports_per_message_results() {
        let pool = test_pool().await;
        let (_, account_id) = seed_user_and_account(&pool).await;
        upsert_folder(&as_db(&pool), &account_id, "INBOX", Some("/"), &[])
            .await
            .unwrap();
        let inbox = get_folder_id(&as_db(&pool), &account_id, "INBOX")
            .await
            .unwrap();

        let (new, updated, persisted) = super::store::persist_jmap_folder_batch(
            &as_db(&pool),
            &account_id,
            &inbox,
            &[
                sample_jmap_email("em1", None),
                sample_jmap_email("em2", None),
            ],
            None,
        )
        .await
        .unwrap();
        assert_eq!((new, updated), (2, 0));
        assert_eq!(persisted.len(), 2, "one result per input email, in order");
        assert!(persisted.iter().all(|p| p.was_new));

        // Re-persist: same external id, now an update with the same local id.
        let (new, updated, persisted2) = super::store::persist_jmap_folder_batch(
            &as_db(&pool),
            &account_id,
            &inbox,
            &[sample_jmap_email("em1", None)],
            None,
        )
        .await
        .unwrap();
        assert_eq!((new, updated), (0, 1));
        assert!(!persisted2[0].was_new);
        assert_eq!(persisted2[0].local_id, persisted[0].local_id);
    }

    #[tokio::test]
    async fn jmap_attachments_persist_to_blob_store() {
        let pool = test_pool().await;
        let (_, account_id) = seed_user_and_account(&pool).await;
        let dir = tempfile::tempdir().unwrap();
        upsert_folder(&as_db(&pool), &account_id, "INBOX", Some("/"), &[])
            .await
            .unwrap();
        let inbox = get_folder_id(&as_db(&pool), &account_id, "INBOX")
            .await
            .unwrap();
        let (_, _, persisted) = super::store::persist_jmap_folder_batch(
            &as_db(&pool),
            &account_id,
            &inbox,
            &[sample_jmap_email("em-att", None)],
            None,
        )
        .await
        .unwrap();
        let local_id = &persisted[0].local_id;

        let extracted = vec![crate::imap::ExtractedAttachment {
            filename: "a.txt".into(),
            content_type: "text/plain".into(),
            data: b"hello blob".to_vec(),
            content_id: None,
            is_inline: false,
        }];
        super::http::persist_attachments(
            &as_db(&pool),
            dir.path(),
            &account_id,
            local_id,
            &extracted,
        )
        .await
        .unwrap();

        let row: (i64, String) = sqlx::query_as(
            "SELECT COUNT(*), MIN(storage_path) FROM attachment WHERE message_id = ?",
        )
        .bind(local_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(row.0, 1);
        let blob = crate::blobs::read(dir.path(), &row.1).await.unwrap();
        assert_eq!(blob, b"hello blob");

        let has: bool = sqlx::query_scalar("SELECT has_attachments FROM message WHERE id = ?")
            .bind(local_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(has);
    }
}
