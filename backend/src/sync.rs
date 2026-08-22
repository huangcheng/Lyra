//! Sync engine module.
//!
//! Orchestrates IMAP adapters, writes to storage, and tracks sync state
//! via the `sync_cursor` table for idempotent, resumable sync.
//!
//! See `docs/specs/2026-08-20-lyra-sync-and-protocols-spec.md`.

#![allow(clippy::doc_markdown)]

use axum::{
    Json, Router,
    body::Body,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use uuid::Uuid;

use crate::auth::AuthState;
use crate::imap::{ImapClient, ImapConfig, ImapError, ImapMessage, ImapSecurity};
use crate::jmap::{JmapClient, JmapError};
use crate::kernel::App;
use crate::protocol::{SyncCtx, SyncOutcome};
use crate::smtp::{OutboundMessage, SmtpAdapter, SmtpConfig, SmtpError, SmtpSecurity};
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
    #[error("message not found")]
    MessageNotFound,
    #[error("account not active or sync disabled")]
    AccountDisabled,
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("imap error: {0}")]
    Imap(#[from] ImapError),
    #[error("jmap error: {0}")]
    Jmap(#[from] JmapError),
    #[error("smtp error: {0}")]
    Smtp(#[from] SmtpError),
    #[error("authentication required")]
    Unauthorized,
    #[error("crypto error: {0}")]
    Crypto(String),
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("protocol error: {0}")]
    Protocol(String),
}

impl IntoResponse for SyncError {
    fn into_response(self) -> axum::response::Response {
        let status = match self {
            SyncError::AccountNotFound | SyncError::MessageNotFound => StatusCode::NOT_FOUND,
            SyncError::AccountDisabled | SyncError::InvalidInput(_) => StatusCode::BAD_REQUEST,
            SyncError::Database(_) | SyncError::Crypto(_) => StatusCode::INTERNAL_SERVER_ERROR,
            SyncError::Imap(_)
            | SyncError::Jmap(_)
            | SyncError::Smtp(_)
            | SyncError::Protocol(_) => StatusCode::BAD_GATEWAY,
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

/// Request to send a message.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendMessageRequest {
    pub account_id: String,
    pub to: Vec<serde_json::Value>,
    pub subject: String,
    pub body_text: Option<String>,
    pub body_html: Option<String>,
    pub cc: Option<Vec<serde_json::Value>>,
    pub bcc: Option<Vec<serde_json::Value>>,
    pub in_reply_to: Option<String>,
    pub references: Option<String>,
}

/// Response for send operation.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SendMessageResponse {
    pub status: String,
    pub message_id: String,
}

/// Routes for sync-related endpoints.
pub fn routes() -> Router<AuthState> {
    Router::new()
        .route("/api/sync/status", get(sync_status))
        .route("/api/accounts/{account_id}/sync", post(trigger_sync))
        .route("/api/messages/send", post(send_message))
        .route("/api/messages/search", get(search_messages))
        .route("/api/messages", get(list_messages_query))
        .route("/api/folders", get(list_folders))
        .route("/api/folders/{folder_id}/messages", get(list_messages))
        .route(
            "/api/messages/{message_id}",
            get(get_message).patch(patch_message),
        )
        .route(
            "/api/messages/{message_id}/attachments",
            get(list_attachments),
        )
        .route("/api/attachments/{attachment_id}", get(download_attachment))
        .route("/api/messages/{message_id}/trash", post(trash_message))
        .route("/api/messages/{message_id}/archive", post(archive_message))
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

    let result = run_account_sync(&state.db, &state.app, &user_id, &account_id).await?;

    Ok(Json(result))
}

/// Send a message via SMTP.
///
/// Loads the account, decrypts credentials, and sends via SMTP.
#[allow(clippy::too_many_lines)]
async fn send_message(
    State(state): State<AuthState>,
    headers: HeaderMap,
    Json(body): Json<SendMessageRequest>,
) -> Result<Json<SendMessageResponse>, SyncError> {
    let user_id = get_user_id(&state, &headers).await?;
    let pool = get_sqlite_pool(state.db());

    // Load account
    let row = sqlx::query(
        r"
        SELECT id, email_address, credential,
               smtp_host, smtp_port, smtp_security, is_active
        FROM mail_account
        WHERE id = ? AND user_id = ?
        ",
    )
    .bind(&body.account_id)
    .bind(&user_id)
    .fetch_optional(pool)
    .await?
    .ok_or(SyncError::AccountNotFound)?;

    let is_active: bool = row.get("is_active");
    if !is_active {
        return Err(SyncError::AccountDisabled);
    }

    let smtp_host: Option<String> = row.get("smtp_host");
    let smtp_port: Option<i32> = row.get("smtp_port");
    let smtp_security: Option<String> = row.get("smtp_security");
    let credential_json: String = row.get("credential");
    let email_address: String = row.get("email_address");

    let host =
        smtp_host.ok_or_else(|| SyncError::InvalidInput("SMTP host not configured".into()))?;
    let port = u16::try_from(smtp_port.unwrap_or(587)).unwrap_or(587);
    let security = match smtp_security.as_deref() {
        Some("tls") => SmtpSecurity::Tls,
        Some("none") => SmtpSecurity::None,
        _ => SmtpSecurity::Starttls,
    };

    // Decrypt password
    let dek =
        crate::auth::AuthState::get_user_dek().map_err(|e| SyncError::Crypto(e.to_string()))?;
    let password = crate::smtp::decrypt_account_password(&credential_json, &dek)?;

    let config = SmtpConfig {
        host,
        port,
        security,
        username: email_address.clone(),
        password,
    };

    // Parse recipients
    let to: Vec<(Option<String>, String)> = body
        .to
        .iter()
        .filter_map(|v| {
            if let Some(s) = v.as_str() {
                Some((None, s.to_string()))
            } else {
                v.get("email").and_then(|e| e.as_str()).map(|email| {
                    let name = v.get("name").and_then(|n| n.as_str()).map(String::from);
                    (name, email.to_string())
                })
            }
        })
        .collect();

    let cc: Vec<(Option<String>, String)> = body
        .cc
        .unwrap_or_default()
        .iter()
        .filter_map(|v| {
            if let Some(s) = v.as_str() {
                Some((None, s.to_string()))
            } else {
                v.get("email").and_then(|e| e.as_str()).map(|email| {
                    let name = v.get("name").and_then(|n| n.as_str()).map(String::from);
                    (name, email.to_string())
                })
            }
        })
        .collect();

    let bcc: Vec<(Option<String>, String)> = body
        .bcc
        .unwrap_or_default()
        .iter()
        .filter_map(|v| {
            if let Some(s) = v.as_str() {
                Some((None, s.to_string()))
            } else {
                v.get("email").and_then(|e| e.as_str()).map(|email| {
                    let name = v.get("name").and_then(|n| n.as_str()).map(String::from);
                    (name, email.to_string())
                })
            }
        })
        .collect();

    if to.is_empty() {
        return Err(SyncError::InvalidInput("No recipients specified".into()));
    }

    let outbound = OutboundMessage {
        from_email: email_address,
        from_name: None,
        to,
        cc,
        bcc,
        subject: body.subject,
        body_text: body.body_text,
        body_html: body.body_html,
        in_reply_to: body.in_reply_to,
        references: body.references,
    };

    let message_id = deliver_smtp(config, outbound).await?;

    Ok(Json(SendMessageResponse {
        status: "sent".into(),
        message_id,
    }))
}

/// Deliver an outbound message through the SMTP adapter.
pub(crate) async fn deliver_smtp(
    config: SmtpConfig,
    outbound: OutboundMessage,
) -> Result<String, SyncError> {
    let adapter = SmtpAdapter::connect(&config)?;
    Ok(adapter.send(&outbound).await?)
}

/// Load SMTP settings for `account_id` and build an outbound message from raw source.
pub(crate) async fn prepare_smtp_send(
    db: &DbPool,
    account_id: &str,
    raw: &str,
) -> Result<(SmtpConfig, OutboundMessage), SyncError> {
    let pool = get_sqlite_pool(db);
    let row = sqlx::query(
        r"
        SELECT email_address, credential,
               smtp_host, smtp_port, smtp_security, is_active
        FROM mail_account
        WHERE id = ?
        ",
    )
    .bind(account_id)
    .fetch_optional(pool)
    .await?
    .ok_or(SyncError::AccountNotFound)?;

    let is_active: bool = row.get("is_active");
    if !is_active {
        return Err(SyncError::AccountDisabled);
    }

    let smtp_host: Option<String> = row.get("smtp_host");
    let smtp_port: Option<i32> = row.get("smtp_port");
    let smtp_security: Option<String> = row.get("smtp_security");
    let credential_json: String = row.get("credential");
    let email_address: String = row.get("email_address");

    let host =
        smtp_host.ok_or_else(|| SyncError::InvalidInput("SMTP host not configured".into()))?;
    let port = u16::try_from(smtp_port.unwrap_or(587)).unwrap_or(587);
    let security = match smtp_security.as_deref() {
        Some("tls") => SmtpSecurity::Tls,
        Some("none") => SmtpSecurity::None,
        _ => SmtpSecurity::Starttls,
    };

    let dek =
        crate::auth::AuthState::get_user_dek().map_err(|e| SyncError::Crypto(e.to_string()))?;
    let password = crate::smtp::decrypt_account_password(&credential_json, &dek)?;

    let to = recipients_from_raw(raw);
    if to.is_empty() {
        return Err(SyncError::InvalidInput("No recipients specified".into()));
    }

    let config = SmtpConfig {
        host,
        port,
        security,
        username: email_address.clone(),
        password,
    };
    let outbound = OutboundMessage {
        from_email: email_address,
        from_name: None,
        to,
        cc: vec![],
        bcc: vec![],
        subject: subject_from_raw(raw).unwrap_or_default(),
        body_text: Some(raw.to_string()),
        body_html: None,
        in_reply_to: None,
        references: None,
    };
    Ok((config, outbound))
}

fn recipients_from_raw(raw: &str) -> Vec<(Option<String>, String)> {
    for line in raw.lines() {
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.eq_ignore_ascii_case("to")
        {
            return value
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| (None, s.to_string()))
                .collect();
        }
    }
    Vec::new()
}

fn subject_from_raw(raw: &str) -> Option<String> {
    for line in raw.lines() {
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.eq_ignore_ascii_case("subject")
        {
            return Some(value.trim().to_string());
        }
    }
    None
}

/// Folder response for the API.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderResponse {
    pub id: String,
    pub account_id: String,
    pub name: String,
    pub role: Option<String>,
    pub sort_order: i32,
    pub total_messages: i32,
    pub unread_messages: i32,
}

/// Message response for the API.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageResponse {
    pub id: String,
    pub account_id: String,
    pub folder_id: String,
    pub subject: Option<String>,
    pub from_address: Option<String>,
    pub to_addresses: Option<String>,
    pub cc_addresses: Option<String>,
    pub date: Option<String>,
    pub snippet: Option<String>,
    pub body_text: Option<String>,
    pub body_html: Option<String>,
    pub is_read: bool,
    pub is_starred: bool,
    pub has_attachments: bool,
}

/// List all folders for the authenticated user.
async fn list_folders(
    State(state): State<AuthState>,
    headers: HeaderMap,
) -> Result<Json<Vec<FolderResponse>>, SyncError> {
    let user_id = get_user_id(&state, &headers).await?;
    let pool = get_sqlite_pool(state.db());

    let rows = sqlx::query(
        r"
        SELECT f.id, f.account_id, f.name, f.role, f.sort_order,
               f.total_messages, f.unread_messages
        FROM folder f
        JOIN mail_account a ON f.account_id = a.id
        WHERE a.user_id = ?
        ORDER BY f.sort_order, f.name
        ",
    )
    .bind(&user_id)
    .fetch_all(pool)
    .await?;

    let folders: Vec<FolderResponse> = rows
        .iter()
        .map(|row| FolderResponse {
            id: row.get("id"),
            account_id: row.get("account_id"),
            name: row.get("name"),
            role: row.get("role"),
            sort_order: row.get("sort_order"),
            total_messages: row.get("total_messages"),
            unread_messages: row.get("unread_messages"),
        })
        .collect();

    Ok(Json(folders))
}

/// List messages in a folder.
async fn list_messages(
    State(state): State<AuthState>,
    Path(folder_id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Vec<MessageResponse>>, SyncError> {
    let user_id = get_user_id(&state, &headers).await?;
    let pool = get_sqlite_pool(state.db());

    // Verify folder belongs to the user
    let _check: String = sqlx::query_scalar(
        r"
        SELECT f.id FROM folder f
        JOIN mail_account a ON f.account_id = a.id
        WHERE f.id = ? AND a.user_id = ?
        ",
    )
    .bind(&folder_id)
    .bind(&user_id)
    .fetch_optional(pool)
    .await?
    .ok_or(SyncError::AccountNotFound)?;

    let rows = sqlx::query(
        r"
        SELECT id, account_id, folder_id, subject, from_address,
               to_addresses, cc_addresses, date, snippet,
               body_text, body_html, is_read, is_starred, has_attachments
        FROM message
        WHERE folder_id = ? AND is_deleted = 0
        ORDER BY date DESC
        LIMIT 500
        ",
    )
    .bind(&folder_id)
    .fetch_all(pool)
    .await?;

    let messages: Vec<MessageResponse> = rows
        .iter()
        .map(|row| MessageResponse {
            id: row.get("id"),
            account_id: row.get("account_id"),
            folder_id: row.get("folder_id"),
            subject: row.get("subject"),
            from_address: row.get("from_address"),
            to_addresses: row.get("to_addresses"),
            cc_addresses: row.get("cc_addresses"),
            date: row.get("date"),
            snippet: row.get("snippet"),
            body_text: row.get("body_text"),
            body_html: row.get("body_html"),
            is_read: row.get::<bool, _>("is_read"),
            is_starred: row.get::<bool, _>("is_starred"),
            has_attachments: row.get::<bool, _>("has_attachments"),
        })
        .collect();

    Ok(Json(messages))
}

/// Query for GET /api/messages (unified inbox / role filter).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListMessagesQuery {
    role: Option<String>,
    account_id: Option<String>,
}

/// List messages across folders, optionally filtered by standard role and account.
async fn list_messages_query(
    State(state): State<AuthState>,
    headers: HeaderMap,
    Query(query): Query<ListMessagesQuery>,
) -> Result<Json<Vec<MessageResponse>>, SyncError> {
    let user_id = get_user_id(&state, &headers).await?;
    let pool = get_sqlite_pool(state.db());

    let rows = sqlx::query(
        r"
        SELECT m.id, m.account_id, m.folder_id, m.subject, m.from_address,
               m.to_addresses, m.cc_addresses, m.date, m.snippet,
               m.body_text, m.body_html, m.is_read, m.is_starred, m.has_attachments
        FROM message m
        JOIN folder f ON m.folder_id = f.id
        JOIN mail_account a ON m.account_id = a.id
        WHERE a.user_id = ?
          AND m.is_deleted = 0
          AND (? IS NULL OR f.role = ?)
          AND (? IS NULL OR m.account_id = ?)
        ORDER BY m.date DESC
        LIMIT 500
        ",
    )
    .bind(&user_id)
    .bind(&query.role)
    .bind(&query.role)
    .bind(&query.account_id)
    .bind(&query.account_id)
    .fetch_all(pool)
    .await?;

    let messages: Vec<MessageResponse> = rows
        .iter()
        .map(|row| MessageResponse {
            id: row.get("id"),
            account_id: row.get("account_id"),
            folder_id: row.get("folder_id"),
            subject: row.get("subject"),
            from_address: row.get("from_address"),
            to_addresses: row.get("to_addresses"),
            cc_addresses: row.get("cc_addresses"),
            date: row.get("date"),
            snippet: row.get("snippet"),
            body_text: row.get("body_text"),
            body_html: row.get("body_html"),
            is_read: row.get::<bool, _>("is_read"),
            is_starred: row.get::<bool, _>("is_starred"),
            has_attachments: row.get::<bool, _>("has_attachments"),
        })
        .collect();

    Ok(Json(messages))
}

/// Query for GET /api/messages/search.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SearchMessagesQuery {
    q: String,
    account_id: Option<String>,
    folder_id: Option<String>,
    limit: Option<i64>,
}

/// Search messages by subject / snippet / body / from (local index).
async fn search_messages(
    State(state): State<AuthState>,
    headers: HeaderMap,
    Query(query): Query<SearchMessagesQuery>,
) -> Result<Json<Vec<MessageResponse>>, SyncError> {
    let user_id = get_user_id(&state, &headers).await?;
    let pool = get_sqlite_pool(state.db());

    let q = query.q.trim();
    if q.is_empty() {
        return Err(SyncError::InvalidInput("q is required".into()));
    }
    if q.chars().count() < 2 {
        return Err(SyncError::InvalidInput(
            "q must be at least 2 characters".into(),
        ));
    }

    let pattern = format!("%{q}%");
    let limit = query.limit.unwrap_or(100).clamp(1, 500);

    let rows = sqlx::query(
        r"
        SELECT m.id, m.account_id, m.folder_id, m.subject, m.from_address,
               m.to_addresses, m.cc_addresses, m.date, m.snippet,
               m.body_text, m.body_html, m.is_read, m.is_starred, m.has_attachments
        FROM message m
        JOIN mail_account a ON m.account_id = a.id
        WHERE a.user_id = ?
          AND m.is_deleted = 0
          AND (? IS NULL OR m.account_id = ?)
          AND (? IS NULL OR m.folder_id = ?)
          AND (
            IFNULL(m.subject, '') LIKE ?
            OR IFNULL(m.snippet, '') LIKE ?
            OR IFNULL(m.body_text, '') LIKE ?
            OR IFNULL(m.from_address, '') LIKE ?
          )
        ORDER BY m.date DESC
        LIMIT ?
        ",
    )
    .bind(&user_id)
    .bind(&query.account_id)
    .bind(&query.account_id)
    .bind(&query.folder_id)
    .bind(&query.folder_id)
    .bind(&pattern)
    .bind(&pattern)
    .bind(&pattern)
    .bind(&pattern)
    .bind(limit)
    .fetch_all(pool)
    .await?;

    let messages: Vec<MessageResponse> = rows
        .iter()
        .map(|row| MessageResponse {
            id: row.get("id"),
            account_id: row.get("account_id"),
            folder_id: row.get("folder_id"),
            subject: row.get("subject"),
            from_address: row.get("from_address"),
            to_addresses: row.get("to_addresses"),
            cc_addresses: row.get("cc_addresses"),
            date: row.get("date"),
            snippet: row.get("snippet"),
            body_text: row.get("body_text"),
            body_html: row.get("body_html"),
            is_read: row.get::<bool, _>("is_read"),
            is_starred: row.get::<bool, _>("is_starred"),
            has_attachments: row.get::<bool, _>("has_attachments"),
        })
        .collect();

    Ok(Json(messages))
}

/// Attachment metadata for list responses.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AttachmentResponse {
    id: String,
    message_id: String,
    filename: Option<String>,
    content_type: Option<String>,
    size_bytes: Option<i64>,
    is_inline: bool,
}

async fn list_attachments(
    State(state): State<AuthState>,
    Path(message_id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Vec<AttachmentResponse>>, SyncError> {
    let user_id = get_user_id(&state, &headers).await?;
    let pool = get_sqlite_pool(state.db());
    let _ = load_message_row(pool, &user_id, &message_id).await?;

    let rows = sqlx::query(
        r"
        SELECT id, message_id, filename, content_type, size_bytes, is_inline
        FROM attachment
        WHERE message_id = ?
        ORDER BY created_at ASC
        ",
    )
    .bind(&message_id)
    .fetch_all(pool)
    .await?;

    Ok(Json(
        rows.iter()
            .map(|row| AttachmentResponse {
                id: row.get("id"),
                message_id: row.get("message_id"),
                filename: row.get("filename"),
                content_type: row.get("content_type"),
                size_bytes: row.get("size_bytes"),
                is_inline: row.get("is_inline"),
            })
            .collect(),
    ))
}

async fn download_attachment(
    State(state): State<AuthState>,
    Path(attachment_id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, SyncError> {
    let user_id = get_user_id(&state, &headers).await?;
    let pool = get_sqlite_pool(state.db());

    let row = sqlx::query(
        r"
        SELECT a.id, a.filename, a.content_type, a.storage_path, a.message_id
        FROM attachment a
        JOIN message m ON a.message_id = m.id
        JOIN mail_account acc ON m.account_id = acc.id
        WHERE a.id = ? AND acc.user_id = ?
        ",
    )
    .bind(&attachment_id)
    .bind(&user_id)
    .fetch_optional(pool)
    .await?
    .ok_or(SyncError::MessageNotFound)?;

    let storage_path: String = row.get("storage_path");
    let filename: Option<String> = row.get("filename");
    let content_type: Option<String> = row.get("content_type");

    let bytes = tokio::fs::read(&storage_path)
        .await
        .map_err(|e| SyncError::InvalidInput(format!("attachment missing on disk: {e}")))?;

    let mut response = Response::new(Body::from(bytes));
    let ct = content_type.unwrap_or_else(|| "application/octet-stream".into());
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&ct)
            .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
    );
    if let Some(name) = filename {
        let safe = name.replace(['"', '\\', '\r', '\n'], "_");
        if let Ok(val) = HeaderValue::from_str(&format!("attachment; filename=\"{safe}\"")) {
            response
                .headers_mut()
                .insert(header::CONTENT_DISPOSITION, val);
        }
    }
    Ok(response)
}

async fn persist_attachments(
    pool: &sqlx::SqlitePool,
    data_dir: &std::path::Path,
    message_id: &str,
    attachments: &[crate::imap::ExtractedAttachment],
) -> Result<(), SyncError> {
    if attachments.is_empty() {
        return Ok(());
    }

    // Replace prior attachment rows for this message (re-fetch).
    sqlx::query("DELETE FROM attachment WHERE message_id = ?")
        .bind(message_id)
        .execute(pool)
        .await?;

    let dir = data_dir.join("attachments").join(message_id);
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| SyncError::InvalidInput(format!("cannot create attachment dir: {e}")))?;

    for att in attachments {
        let id = Uuid::new_v7(uuid::Timestamp::now(uuid::NoContext)).to_string();
        let path = dir.join(&id);
        tokio::fs::write(&path, &att.data)
            .await
            .map_err(|e| SyncError::InvalidInput(format!("cannot write attachment: {e}")))?;

        let size = i64::try_from(att.data.len()).unwrap_or(i64::MAX);
        sqlx::query(
            r"
            INSERT INTO attachment (
                id, message_id, filename, content_type, size_bytes,
                storage_path, content_id, is_inline
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)
            ",
        )
        .bind(&id)
        .bind(message_id)
        .bind(&att.filename)
        .bind(&att.content_type)
        .bind(size)
        .bind(path.to_string_lossy().as_ref())
        .bind(&att.content_id)
        .bind(att.is_inline)
        .execute(pool)
        .await?;
    }

    sqlx::query(
        "UPDATE message SET has_attachments = 1, updated_at = datetime('now') WHERE id = ?",
    )
    .bind(message_id)
    .execute(pool)
    .await?;

    Ok(())
}

/// Request body for PATCH /api/messages/{id}.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PatchMessageRequest {
    is_read: Option<bool>,
    is_starred: Option<bool>,
}

/// Loaded message row with account/folder context for mutations.
struct MessageRow {
    id: String,
    account_id: String,
    folder_id: String,
    folder_name: String,
    external_id: Option<String>,
    protocol: String,
    body_text: Option<String>,
    body_html: Option<String>,
    is_read: bool,
    is_starred: bool,
    subject: Option<String>,
    from_address: Option<String>,
    to_addresses: Option<String>,
    cc_addresses: Option<String>,
    date: Option<String>,
    snippet: Option<String>,
    has_attachments: bool,
}

fn message_response_from_row(row: &MessageRow) -> MessageResponse {
    MessageResponse {
        id: row.id.clone(),
        account_id: row.account_id.clone(),
        folder_id: row.folder_id.clone(),
        subject: row.subject.clone(),
        from_address: row.from_address.clone(),
        to_addresses: row.to_addresses.clone(),
        cc_addresses: row.cc_addresses.clone(),
        date: row.date.clone(),
        snippet: row.snippet.clone(),
        body_text: row.body_text.clone(),
        body_html: row.body_html.clone(),
        is_read: row.is_read,
        is_starred: row.is_starred,
        has_attachments: row.has_attachments,
    }
}

async fn load_message_row(
    pool: &sqlx::SqlitePool,
    user_id: &str,
    message_id: &str,
) -> Result<MessageRow, SyncError> {
    let row = sqlx::query(
        r"
        SELECT m.id, m.account_id, m.folder_id, m.external_id,
               m.subject, m.from_address, m.to_addresses, m.cc_addresses,
               m.date, m.snippet, m.body_text, m.body_html,
               m.is_read, m.is_starred, m.has_attachments,
               f.external_id AS folder_name,
               a.protocol
        FROM message m
        JOIN folder f ON m.folder_id = f.id
        JOIN mail_account a ON m.account_id = a.id
        WHERE m.id = ? AND a.user_id = ? AND m.is_deleted = 0
        ",
    )
    .bind(message_id)
    .bind(user_id)
    .fetch_optional(pool)
    .await?
    .ok_or(SyncError::MessageNotFound)?;

    Ok(MessageRow {
        id: row.get("id"),
        account_id: row.get("account_id"),
        folder_id: row.get("folder_id"),
        folder_name: row.get("folder_name"),
        external_id: row.get("external_id"),
        protocol: row.get("protocol"),
        body_text: row.get("body_text"),
        body_html: row.get("body_html"),
        is_read: row.get("is_read"),
        is_starred: row.get("is_starred"),
        subject: row.get("subject"),
        from_address: row.get("from_address"),
        to_addresses: row.get("to_addresses"),
        cc_addresses: row.get("cc_addresses"),
        date: row.get("date"),
        snippet: row.get("snippet"),
        has_attachments: row.get("has_attachments"),
    })
}

async fn connect_imap_for_account(
    pool: &sqlx::SqlitePool,
    user_id: &str,
    account_id: &str,
) -> Result<(ImapClient, String), SyncError> {
    let row = sqlx::query(
        r"
        SELECT email_address, credential, imap_host, imap_port, imap_security, protocol
        FROM mail_account
        WHERE id = ? AND user_id = ? AND is_active = 1
        ",
    )
    .bind(account_id)
    .bind(user_id)
    .fetch_optional(pool)
    .await?
    .ok_or(SyncError::AccountNotFound)?;

    let protocol: String = row.get("protocol");
    if protocol != "imap" {
        return Err(SyncError::InvalidInput(
            "remote flag/move ops require an IMAP account in v1".into(),
        ));
    }

    let imap_host: Option<String> = row.get("imap_host");
    let imap_port: Option<i32> = row.get("imap_port");
    let imap_security: Option<String> = row.get("imap_security");
    let email_address: String = row.get("email_address");
    let credential_json: String = row.get("credential");

    let host =
        imap_host.ok_or_else(|| SyncError::InvalidInput("IMAP host not configured".into()))?;
    let port = u16::try_from(imap_port.unwrap_or(993)).unwrap_or(993);
    let security = match imap_security.as_deref() {
        Some("starttls") => ImapSecurity::Starttls,
        Some("none") => ImapSecurity::None,
        _ => ImapSecurity::Tls,
    };

    let dek =
        crate::auth::AuthState::get_user_dek().map_err(|e| SyncError::Crypto(e.to_string()))?;
    let password = crate::imap::decrypt_account_password(&credential_json, &dek)
        .map_err(|e| SyncError::Crypto(e.to_string()))?;

    let client = ImapClient::connect(&ImapConfig {
        host,
        port,
        security,
        username: email_address,
        password,
    })
    .await?;

    Ok((client, protocol))
}

fn parse_imap_uid(external_id: Option<&str>) -> Result<u32, SyncError> {
    external_id
        .and_then(|s| s.parse::<u32>().ok())
        .ok_or_else(|| SyncError::InvalidInput("message has no IMAP UID".into()))
}

/// GET /api/messages/{id} — return one message; lazily fetch IMAP body if missing.
async fn get_message(
    State(state): State<AuthState>,
    Path(message_id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<MessageResponse>, SyncError> {
    let user_id = get_user_id(&state, &headers).await?;
    let pool = get_sqlite_pool(state.db());
    let mut row = load_message_row(pool, &user_id, &message_id).await?;

    let needs_body = row.body_text.is_none() && row.body_html.is_none();
    if needs_body
        && row.protocol == "imap"
        && let Ok(uid) = parse_imap_uid(row.external_id.as_deref())
    {
        let (mut client, _) = connect_imap_for_account(pool, &user_id, &row.account_id).await?;
        client.select(&row.folder_name).await?;
        let bodies = client.fetch_bodies(&[uid]).await?;
        if let Some(fetched) = bodies.into_iter().next() {
            sqlx::query(
                r"
                    UPDATE message
                    SET body_text = ?, body_html = ?, has_attachments = ?,
                        snippet = COALESCE(snippet, ?),
                        updated_at = datetime('now')
                    WHERE id = ?
                    ",
            )
            .bind(&fetched.body_text)
            .bind(&fetched.body_html)
            .bind(fetched.has_attachments)
            .bind(
                fetched
                    .body_text
                    .as_deref()
                    .map(|t| t.chars().take(120).collect::<String>()),
            )
            .bind(&row.id)
            .execute(pool)
            .await?;

            row.body_text = fetched.body_text;
            row.body_html = fetched.body_html;
            row.has_attachments = fetched.has_attachments || !fetched.attachments.is_empty();
            persist_attachments(pool, &state.data_dir, &row.id, &fetched.attachments).await?;
        }
    }

    Ok(Json(message_response_from_row(&row)))
}

/// PATCH /api/messages/{id} — update read/starred flags (IMAP STORE when possible).
async fn patch_message(
    State(state): State<AuthState>,
    Path(message_id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<PatchMessageRequest>,
) -> Result<Json<MessageResponse>, SyncError> {
    let user_id = get_user_id(&state, &headers).await?;
    let pool = get_sqlite_pool(state.db());
    let mut row = load_message_row(pool, &user_id, &message_id).await?;

    if body.is_read.is_none() && body.is_starred.is_none() {
        return Err(SyncError::InvalidInput(
            "provide isRead and/or isStarred".into(),
        ));
    }

    let next_read = body.is_read.unwrap_or(row.is_read);
    let next_star = body.is_starred.unwrap_or(row.is_starred);

    if row.protocol == "imap" && (body.is_read.is_some() || body.is_starred.is_some()) {
        let uid = parse_imap_uid(row.external_id.as_deref())?;
        let (mut client, _) = connect_imap_for_account(pool, &user_id, &row.account_id).await?;
        client.select(&row.folder_name).await?;

        if let Some(is_read) = body.is_read {
            if is_read && !row.is_read {
                client.add_flags(uid, &["\\Seen"]).await?;
            } else if !is_read && row.is_read {
                client.remove_flags(uid, &["\\Seen"]).await?;
            }
        }
        if let Some(is_starred) = body.is_starred {
            if is_starred && !row.is_starred {
                client.add_flags(uid, &["\\Flagged"]).await?;
            } else if !is_starred && row.is_starred {
                client.remove_flags(uid, &["\\Flagged"]).await?;
            }
        }
    }

    sqlx::query(
        r"
        UPDATE message
        SET is_read = ?, is_starred = ?, updated_at = datetime('now')
        WHERE id = ?
        ",
    )
    .bind(next_read)
    .bind(next_star)
    .bind(&row.id)
    .execute(pool)
    .await?;

    row.is_read = next_read;
    row.is_starred = next_star;

    // Keep folder unread counts roughly consistent.
    update_folder_counts(pool, &row.folder_id).await?;

    Ok(Json(message_response_from_row(&row)))
}

/// POST /api/messages/{id}/trash — move to Trash (IMAP) or soft-delete locally.
async fn trash_message(
    State(state): State<AuthState>,
    Path(message_id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, SyncError> {
    move_message_to_role(state, message_id, headers, "trash").await
}

/// POST /api/messages/{id}/archive — move to Archive when available.
async fn archive_message(
    State(state): State<AuthState>,
    Path(message_id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, SyncError> {
    move_message_to_role(state, message_id, headers, "archive").await
}

async fn move_message_to_role(
    state: AuthState,
    message_id: String,
    headers: HeaderMap,
    role: &str,
) -> Result<Json<serde_json::Value>, SyncError> {
    let user_id = get_user_id(&state, &headers).await?;
    let pool = get_sqlite_pool(state.db());
    let row = load_message_row(pool, &user_id, &message_id).await?;

    let dest = sqlx::query(
        r"
        SELECT id, external_id, name FROM folder
        WHERE account_id = ? AND role = ?
        ORDER BY sort_order ASC
        LIMIT 1
        ",
    )
    .bind(&row.account_id)
    .bind(role)
    .fetch_optional(pool)
    .await?;

    let Some(dest) = dest else {
        // No destination folder: soft-delete locally.
        sqlx::query("UPDATE message SET is_deleted = 1, updated_at = datetime('now') WHERE id = ?")
            .bind(&row.id)
            .execute(pool)
            .await?;
        update_folder_counts(pool, &row.folder_id).await?;
        return Ok(Json(serde_json::json!({
            "status": "ok",
            "action": "soft_delete",
            "role": role,
        })));
    };

    let dest_id: String = dest.get("id");
    let dest_name: String = dest
        .get::<Option<String>, _>("external_id")
        .unwrap_or_else(|| dest.get("name"));

    if row.protocol == "imap" {
        let uid = parse_imap_uid(row.external_id.as_deref())?;
        let (mut client, _) = connect_imap_for_account(pool, &user_id, &row.account_id).await?;
        client.select(&row.folder_name).await?;
        client.move_uid(uid, &dest_name).await?;
    }

    sqlx::query(
        r"
        UPDATE message
        SET folder_id = ?, updated_at = datetime('now')
        WHERE id = ?
        ",
    )
    .bind(&dest_id)
    .bind(&row.id)
    .execute(pool)
    .await?;

    update_folder_counts(pool, &row.folder_id).await?;
    update_folder_counts(pool, &dest_id).await?;

    Ok(Json(serde_json::json!({
        "status": "ok",
        "action": "moved",
        "role": role,
        "folderId": dest_id,
    })))
}

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
    let pool = get_sqlite_pool(db);

    let row = sqlx::query(
        r"
        SELECT receive_protocol, protocol, is_active, sync_enabled
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

    let receive_protocol: Option<String> = row.get("receive_protocol");
    let protocol: String = row.get("protocol");
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

    sqlx::query("UPDATE mail_account SET last_sync_at = datetime('now'), updated_at = datetime('now') WHERE id = ?")
        .bind(account_id)
        .execute(pool)
        .await?;

    Ok(SyncResponse {
        account_id: account_id.to_string(),
        status: "completed".into(),
        folders_synced: usize::try_from(outcome.folders_synced).unwrap_or(usize::MAX),
        messages_synced: usize::try_from(outcome.messages_synced).unwrap_or(usize::MAX),
        messages_updated: 0,
        messages_deleted: 0,
    })
}

/// Load an IMAP account and run the existing IMAP fetch loop.
pub(crate) async fn imap_sync_account(
    db: &DbPool,
    user_id: &str,
    account_id: &str,
) -> Result<SyncOutcome, SyncError> {
    let pool = get_sqlite_pool(db);
    let row = load_account_sync_row(pool, user_id, account_id).await?;
    let credential_json: String = row.get("credential");
    let dek =
        crate::auth::AuthState::get_user_dek().map_err(|e| SyncError::Crypto(e.to_string()))?;
    let password = crate::imap::decrypt_account_password(&credential_json, &dek)?;
    let result = run_imap_sync(pool, account_id, &row, &password).await?;
    Ok(outcome_from_response(&result))
}

/// Load a JMAP account and run the existing JMAP fetch loop.
///
/// JMAP-then-IMAP fallback stays inside this plugin path, not core dispatch.
pub(crate) async fn jmap_sync_account(
    db: &DbPool,
    user_id: &str,
    account_id: &str,
) -> Result<SyncOutcome, SyncError> {
    let pool = get_sqlite_pool(db);
    let row = load_account_sync_row(pool, user_id, account_id).await?;
    let credential_json: String = row.get("credential");
    let email_address: String = row.get("email_address");
    let jmap_base_url: Option<String> = row.get("jmap_base_url");
    let dek =
        crate::auth::AuthState::get_user_dek().map_err(|e| SyncError::Crypto(e.to_string()))?;

    let result = if let Some(ref base_url) = jmap_base_url {
        let password = crate::jmap::decrypt_account_password(&credential_json, &dek)?;
        match run_jmap_sync(pool, account_id, base_url, &email_address, &password).await {
            Ok(result) => result,
            Err(e) => {
                tracing::warn!("JMAP sync failed ({e}), falling back to IMAP");
                let password = crate::imap::decrypt_account_password(&credential_json, &dek)?;
                run_imap_sync(pool, account_id, &row, &password).await?
            }
        }
    } else {
        let password = crate::imap::decrypt_account_password(&credential_json, &dek)?;
        run_imap_sync(pool, account_id, &row, &password).await?
    };
    Ok(outcome_from_response(&result))
}

async fn load_account_sync_row(
    pool: &sqlx::SqlitePool,
    user_id: &str,
    account_id: &str,
) -> Result<sqlx::sqlite::SqliteRow, SyncError> {
    sqlx::query(
        r"
        SELECT id, email_address, protocol, credential,
               imap_host, imap_port, imap_security,
               jmap_base_url,
               is_active, sync_enabled
        FROM mail_account
        WHERE id = ? AND user_id = ?
        ",
    )
    .bind(account_id)
    .bind(user_id)
    .fetch_optional(pool)
    .await?
    .ok_or(SyncError::AccountNotFound)
}

fn outcome_from_response(result: &SyncResponse) -> SyncOutcome {
    SyncOutcome {
        folders_synced: u32::try_from(result.folders_synced).unwrap_or(u32::MAX),
        messages_synced: u32::try_from(result.messages_synced).unwrap_or(u32::MAX),
    }
}

/// Run an IMAP-based sync for an account.
///
/// Connects to the IMAP server, lists folders, and syncs messages
/// using UIDVALIDITY + UID cursors.
#[allow(clippy::too_many_lines)]
pub(crate) async fn run_imap_sync(
    pool: &sqlx::SqlitePool,
    account_id: &str,
    row: &sqlx::sqlite::SqliteRow,
    password: &str,
) -> Result<SyncResponse, SyncError> {
    let imap_host: Option<String> = row.get("imap_host");
    let imap_port: Option<i32> = row.get("imap_port");
    let imap_security: Option<String> = row.get("imap_security");
    let email_address: String = row.get("email_address");

    let host = imap_host.ok_or_else(|| SyncError::Crypto("IMAP host not configured".into()))?;
    let port = u16::try_from(imap_port.unwrap_or(993)).unwrap_or(993);
    let security = match imap_security.as_deref() {
        Some("starttls") => ImapSecurity::Starttls,
        Some("none") => ImapSecurity::None,
        _ => ImapSecurity::Tls,
    };

    let config = ImapConfig {
        host,
        port,
        security,
        username: email_address,
        password: password.to_string(),
    };

    // Connect to IMAP
    let mut client = ImapClient::connect(&config).await?;

    // Sync folders
    let folders = client.list_folders().await?;
    let mut folders_synced = 0;

    for folder in &folders {
        upsert_folder(pool, account_id, &folder.name, folder.delimiter.as_deref()).await?;
        folders_synced += 1;
    }

    // Sync messages in each folder
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

/// Run a JMAP sync for an account.
///
/// Discovers the JMAP session, lists mailboxes, queries emails,
/// and upserts them into the database.
pub(crate) async fn run_jmap_sync(
    pool: &sqlx::SqlitePool,
    account_id: &str,
    jmap_base_url: &str,
    email: &str,
    password: &str,
) -> Result<SyncResponse, SyncError> {
    // 1. Discover JMAP session
    let client = JmapClient::discover(jmap_base_url, email, password).await?;

    // 2. List mailboxes
    let mailboxes = client.list_mailboxes().await?;
    let mut folders_synced = 0;

    for mb in &mailboxes {
        upsert_folder(pool, account_id, &mb.name, mb.role.as_deref()).await?;
        folders_synced += 1;
    }

    // 3. Sync emails per mailbox
    let mut total_new = 0;
    let mut total_updated = 0;

    for mb in &mailboxes {
        let folder_id = get_folder_id(pool, account_id, &mb.name).await?;

        // Load stored JMAP state
        let cursor = load_cursor(pool, account_id, &folder_id).await?;
        let since_state = cursor.as_ref().and_then(|c| {
            // JMAP uses state tokens, not UIDs
            if c.uid_validity == 0 {
                // This is a JMAP cursor (stored with uid_validity=0)
                Some(c.last_uid.to_string()) // We store the query state hash as last_uid
            } else {
                None // IMAP cursor, start fresh
            }
        });

        // Query emails
        let query_result = client
            .query_emails(&mb.id, since_state.as_deref(), Some(100))
            .await?;

        if query_result.ids.is_empty() {
            continue;
        }

        // Fetch email objects
        let emails = client.get_emails(&query_result.ids).await?;

        // Upsert each email
        for email_obj in &emails {
            let was_new = upsert_jmap_message(pool, account_id, &folder_id, email_obj).await?;
            if was_new {
                total_new += 1;
            } else {
                total_updated += 1;
            }
        }

        // Save JMAP cursor (use a hash of the query state for idempotency)
        if let Some(ref qs) = query_result.query_state {
            let state_hash = {
                use std::collections::hash_map::DefaultHasher;
                use std::hash::{Hash, Hasher};
                let mut h = DefaultHasher::new();
                qs.hash(&mut h);
                #[allow(clippy::cast_possible_truncation)]
                {
                    h.finish() as u32
                }
            };
            save_cursor(pool, account_id, &folder_id, "jmap", 0, state_hash).await?;
        }

        update_folder_counts(pool, &folder_id).await?;
    }

    Ok(SyncResponse {
        account_id: account_id.to_string(),
        status: "completed".into(),
        folders_synced,
        messages_synced: total_new,
        messages_updated: total_updated,
        messages_deleted: 0,
    })
}

/// Upsert a message from JMAP email data.
///
/// Returns `true` if the message was newly inserted, `false` if updated.
async fn upsert_jmap_message(
    pool: &sqlx::SqlitePool,
    account_id: &str,
    folder_id: &str,
    email: &crate::jmap::JmapEmail,
) -> Result<bool, SyncError> {
    let external_id = &email.id;

    let existing: Option<String> =
        sqlx::query_scalar("SELECT id FROM message WHERE account_id = ? AND external_id = ?")
            .bind(account_id)
            .bind(external_id)
            .fetch_optional(pool)
            .await?;

    let is_read = email.is_seen();
    let is_starred = email.is_flagged();
    let snippet = email.preview.clone().or_else(|| {
        email.subject.as_ref().map(|s| {
            if s.len() > 120 {
                format!("{}...", &s[..117])
            } else {
                s.clone()
            }
        })
    });

    let from_json = email
        .format_from()
        .map(|f| serde_json::json!({ "raw": f }).to_string());
    let to_json = email
        .to_string_list()
        .map(|t| serde_json::json!(vec![t]).to_string());
    let cc_json = email.cc.as_ref().map(|addrs| {
        let formatted: Vec<String> = addrs
            .iter()
            .map(|a| match (&a.name, &a.email) {
                (Some(name), Some(email)) => format!("{name} <{email}>"),
                (None, Some(email)) => email.clone(),
                _ => String::new(),
            })
            .collect();
        serde_json::json!(formatted).to_string()
    });

    let flags_json = serde_json::to_string(&email.keywords).unwrap_or_else(|_| "{}".into());

    if let Some(id) = existing {
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
        let id = uuid::Uuid::new_v7(uuid::Timestamp::now(uuid::NoContext)).to_string();

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
        .bind(external_id)
        .bind(email.message_id_header())
        .bind(&email.subject)
        .bind(&from_json)
        .bind(&to_json)
        .bind(&cc_json)
        .bind(&email.received_at)
        .bind(is_read)
        .bind(is_starred)
        .bind(&flags_json)
        .bind(email.size.map(|s| i32::try_from(s).unwrap_or(i32::MAX)))
        .bind(
            email
                .in_reply_to
                .as_ref()
                .and_then(|ids| ids.first())
                .cloned(),
        )
        .bind(email.references.as_ref().map(|refs| refs.join(" ")))
        .bind(&snippet)
        .bind(email.has_attachment.unwrap_or(false))
        .bind(email.body_text())
        .bind(email.body_html())
        .execute(pool)
        .await?;

        Ok(true)
    }
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
    let total: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM message WHERE folder_id = ? AND is_deleted = 0")
            .bind(folder_id)
            .fetch_one(pool)
            .await?;

    let unread: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM message WHERE folder_id = ? AND is_deleted = 0 AND is_read = 0",
    )
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
            attachments: vec![],
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
            attachments: vec![],
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
                attachments: vec![],
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
