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
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use uuid::Uuid;

use crate::auth::{AuthState, AuthUser};
use crate::imap::{ImapClient, ImapConfig, ImapError, ImapMessage, ImapSecurity};
use crate::jmap::{JmapClient, JmapError};
use crate::kernel::App;
use crate::protocol::{SendHandle, SyncCtx, SyncOutcome};
use crate::smtp::{OutboundMessage, SmtpAdapter, SmtpConfig, SmtpError, SmtpSecurity};
use crate::storage::DbPool;

/// Look up a send plugin by `send_protocol`. Unknown ids map to HTTP 400.
fn resolve_send_plugin(app: &App, send_protocol: &str) -> Result<SendHandle, SyncError> {
    app.send(send_protocol)
        .map_err(|e| SyncError::InvalidInput(e.to_string()))
}

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

/// HTTP 202 body when a sync job is enqueued.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnqueuedSync {
    pub job_id: String,
    pub status: String,
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
    #[error("crypto error: {0}")]
    Crypto(String),
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("protocol error: {0}")]
    Protocol(String),
}

impl IntoResponse for SyncError {
    fn into_response(self) -> axum::response::Response {
        // 5xx/upstream variants carry hostnames, SQL detail, and protocol
        // chatter: log the full error server-side, answer "internal error".
        // 4xx variants are deliberate API surface and stay descriptive.
        let (status, message) = match &self {
            SyncError::AccountNotFound | SyncError::MessageNotFound => {
                (StatusCode::NOT_FOUND, self.to_string())
            }
            SyncError::AccountDisabled | SyncError::InvalidInput(_) => {
                (StatusCode::BAD_REQUEST, self.to_string())
            }
            // Crypto messages carry deliberate operator guidance, no secrets.
            SyncError::Crypto(_) => (StatusCode::INTERNAL_SERVER_ERROR, self.to_string()),
            masked @ (SyncError::Database(_)
            | SyncError::Imap(_)
            | SyncError::Jmap(_)
            | SyncError::Smtp(_)
            | SyncError::Protocol(_)) => {
                tracing::error!(error = %masked, "sync request failed");
                let status = if matches!(masked, SyncError::Database(_)) {
                    StatusCode::INTERNAL_SERVER_ERROR
                } else {
                    StatusCode::BAD_GATEWAY
                };
                (status, "internal error".to_string())
            }
        };
        (status, Json(serde_json::json!({ "error": message }))).into_response()
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
        .route("/api/v1/sync/status", get(sync_status))
        .route("/api/v1/accounts/{account_id}/sync", post(trigger_sync))
        .route("/api/v1/messages/send", post(send_message))
        .route("/api/v1/messages/search", get(search_messages))
        .route("/api/v1/messages", get(list_messages_query))
        .route("/api/v1/folders", get(list_folders))
        .route("/api/v1/folders/{folder_id}/messages", get(list_messages))
        .route(
            "/api/v1/messages/{message_id}",
            get(get_message).patch(patch_message),
        )
        .route(
            "/api/v1/messages/{message_id}/attachments",
            get(list_attachments),
        )
        .route(
            "/api/v1/attachments/{attachment_id}",
            get(download_attachment),
        )
        .route("/api/v1/messages/{message_id}/trash", post(trash_message))
        .route(
            "/api/v1/messages/{message_id}/archive",
            post(archive_message),
        )
        .route("/api/v1/messages/{message_id}/snooze", post(snooze_message))
}

// ── Handlers ────────────────────────────────────────────────────────

/// Get sync status.
async fn sync_status(
    State(state): State<AuthState>,
    AuthUser(user_id): AuthUser,
) -> Result<Json<SyncStatus>, SyncError> {
    let db = state.db();

    let count: i64 = db_scalar!(
        db,
        i64,
        "SELECT COUNT(*) FROM mail_account WHERE user_id = ? AND is_active = 1 AND sync_enabled = 1",
        &user_id
    )?;

    // Pending or running sync_account jobs for this user (payload JSON has user_id).
    let syncing = user_has_active_sync_job(db, &user_id).await?;

    Ok(Json(SyncStatus {
        active_accounts: count,
        syncing,
    }))
}

/// True when any `sync_account` job for `user_id` is pending or running.
async fn user_has_active_sync_job(db: &DbPool, user_id: &str) -> Result<bool, SyncError> {
    let rows = db_fetch_all!(
        db,
        r"
        SELECT payload FROM jobs
        WHERE kind = 'sync_account' AND status IN ('pending', 'running')
        ",
        |row| row.get::<String, _>("payload")
    )?;

    for payload_json in rows {
        let Ok(payload) = serde_json::from_str::<crate::jobs::JobPayload>(&payload_json) else {
            continue;
        };
        if let crate::jobs::JobPayload::SyncAccount {
            user_id: job_user, ..
        } = payload
            && job_user == user_id
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Trigger a sync for a specific account (enqueue; do not await IMAP).
async fn trigger_sync(
    State(state): State<AuthState>,
    Path(account_id): Path<String>,
    AuthUser(user_id): AuthUser,
) -> Result<(StatusCode, Json<EnqueuedSync>), SyncError> {
    let db = state.db();

    let exists: Option<String> = db_scalar_optional!(
        db,
        String,
        "SELECT id FROM mail_account WHERE id = ? AND user_id = ?",
        &account_id,
        &user_id
    )?;
    if exists.is_none() {
        return Err(SyncError::AccountNotFound);
    }

    // Prefer 202 + existing job id over 409 so the Settings poller can keep
    // polling the same job instead of treating a duplicate trigger as an error.
    if let Some(job_id) = crate::scheduler::pending_or_running_sync_job_id(db, &account_id).await? {
        return Ok((
            StatusCode::ACCEPTED,
            Json(EnqueuedSync {
                job_id,
                status: "queued".into(),
            }),
        ));
    }

    let now = chrono::Utc::now().to_rfc3339();
    let job_id = crate::jobs::enqueue(
        db,
        &crate::jobs::JobPayload::SyncAccount {
            account_id,
            user_id,
        },
        &now,
    )
    .await?;

    Ok((
        StatusCode::ACCEPTED,
        Json(EnqueuedSync {
            job_id,
            status: "queued".into(),
        }),
    ))
}

/// Send a message through the account's `SendPlugin` (SMTP today).
///
/// Mailbox sync is queued for workers; single-message SMTP stays request-scoped
/// so compose can await success without a 202/`jobId` handshake.
#[allow(clippy::too_many_lines)]
async fn send_message(
    State(state): State<AuthState>,
    AuthUser(user_id): AuthUser,
    Json(body): Json<SendMessageRequest>,
) -> Result<Json<SendMessageResponse>, SyncError> {
    let db = state.db();

    let row = db_fetch_optional!(
        db,
        r"
        SELECT email_address, send_protocol, is_active
        FROM mail_account
        WHERE id = ? AND user_id = ?
        ",
        |row| {
            let is_active: bool = row.get("is_active");
            let send_protocol: String = row.get("send_protocol");
            let email_address: String = row.get("email_address");
            (is_active, send_protocol, email_address)
        },
        &body.account_id,
        &user_id
    )?
    .ok_or(SyncError::AccountNotFound)?;

    let (is_active, send_protocol, email_address) = row;
    if !is_active {
        return Err(SyncError::AccountDisabled);
    }

    let plugin = resolve_send_plugin(state.app.as_ref(), &send_protocol)?;

    let to = parse_address_list(&body.to);
    let cc = parse_address_list(&body.cc.unwrap_or_default());
    let bcc = parse_address_list(&body.bcc.unwrap_or_default());

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

    let raw = serde_json::to_string(&outbound)
        .map_err(|e| SyncError::InvalidInput(format!("cannot encode outbound: {e}")))?;

    plugin
        .send(&body.account_id, &raw)
        .await
        .map_err(SyncError::Protocol)?;

    let message_id = format!(
        "sent-{}",
        Uuid::new_v7(uuid::Timestamp::now(uuid::NoContext))
    );

    Ok(Json(SendMessageResponse {
        status: "sent".into(),
        message_id,
    }))
}

fn parse_address_list(values: &[serde_json::Value]) -> Vec<(Option<String>, String)> {
    values
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
        .collect()
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
///
/// `raw` may be JSON [`OutboundMessage`] (HTTP compose) or minimal RFC822-ish text.
pub(crate) async fn prepare_smtp_send(
    db: &DbPool,
    account_id: &str,
    raw: &str,
) -> Result<(SmtpConfig, OutboundMessage), SyncError> {
    let row = db_fetch_optional!(
        db,
        r"
        SELECT email_address, credential, user_id,
               smtp_host, smtp_port, smtp_security, is_active
        FROM mail_account
        WHERE id = ?
        ",
        |row| {
            let is_active: bool = row.get("is_active");
            let smtp_host: Option<String> = row.get("smtp_host");
            let smtp_port: Option<i32> = row.get("smtp_port");
            let smtp_security: Option<String> = row.get("smtp_security");
            let credential_json: String = row.get("credential");
            let email_address: String = row.get("email_address");
            let user_id: String = row.get("user_id");
            (
                is_active,
                smtp_host,
                smtp_port,
                smtp_security,
                credential_json,
                email_address,
                user_id,
            )
        },
        account_id
    )?
    .ok_or(SyncError::AccountNotFound)?;

    let (is_active, smtp_host, smtp_port, smtp_security, credential_json, email_address, user_id) =
        row;
    if !is_active {
        return Err(SyncError::AccountDisabled);
    }

    let host =
        smtp_host.ok_or_else(|| SyncError::InvalidInput("SMTP host not configured".into()))?;
    let port = u16::try_from(smtp_port.unwrap_or(587)).unwrap_or(587);
    let security = match smtp_security.as_deref() {
        Some(s) => {
            match crate::netsec::normalize_security_mode(s).map_err(SyncError::InvalidInput)? {
                "tls" => SmtpSecurity::Tls,
                _ => SmtpSecurity::Starttls,
            }
        }
        None => SmtpSecurity::Starttls,
    };

    let dek = crate::auth::AuthState::get_user_dek(db, &user_id)
        .await
        .map_err(|e| SyncError::Crypto(e.to_string()))?;
    let password = crate::smtp::decrypt_account_password(&credential_json, &dek)?;

    let config = SmtpConfig {
        host,
        port,
        security,
        username: email_address.clone(),
        password: zeroize::Zeroizing::new(password),
    };

    let outbound = outbound_from_raw(email_address, raw)?;
    Ok((config, outbound))
}

fn outbound_from_raw(from_email: String, raw: &str) -> Result<OutboundMessage, SyncError> {
    let trimmed = raw.trim_start();
    if trimmed.starts_with('{') {
        let mut outbound: OutboundMessage = serde_json::from_str(trimmed)
            .map_err(|e| SyncError::InvalidInput(format!("invalid outbound JSON: {e}")))?;
        outbound.from_email = from_email;
        if outbound.to.is_empty() {
            return Err(SyncError::InvalidInput("No recipients specified".into()));
        }
        return Ok(outbound);
    }

    let to = recipients_from_raw(raw);
    if to.is_empty() {
        return Err(SyncError::InvalidInput("No recipients specified".into()));
    }

    Ok(OutboundMessage {
        from_email,
        from_name: None,
        to,
        cc: vec![],
        bcc: vec![],
        subject: subject_from_raw(raw).unwrap_or_default(),
        body_text: Some(body_from_raw(raw)),
        body_html: None,
        in_reply_to: None,
        references: None,
    })
}

fn body_from_raw(raw: &str) -> String {
    if let Some(idx) = raw.find("\n\n") {
        raw[idx + 2..].to_string()
    } else if let Some(idx) = raw.find("\r\n\r\n") {
        raw[idx + 4..].to_string()
    } else {
        raw.to_string()
    }
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

macro_rules! message_response_from_sql {
    ($row:expr) => {{
        MessageResponse {
            id: $row.get("id"),
            account_id: $row.get("account_id"),
            folder_id: $row.get("folder_id"),
            subject: $row.get("subject"),
            from_address: $row.get("from_address"),
            to_addresses: $row.get("to_addresses"),
            cc_addresses: $row.get("cc_addresses"),
            date: $row.get("date"),
            snippet: $row.get("snippet"),
            body_text: $row.get("body_text"),
            body_html: $row.get("body_html"),
            is_read: $row.get::<bool, _>("is_read"),
            is_starred: $row.get::<bool, _>("is_starred"),
            has_attachments: $row.get::<bool, _>("has_attachments"),
        }
    }};
}

/// List all folders for the authenticated user.
async fn list_folders(
    State(state): State<AuthState>,
    AuthUser(user_id): AuthUser,
) -> Result<Json<Vec<FolderResponse>>, SyncError> {
    let db = state.db();

    let folders = db_fetch_all!(
        db,
        r"
        SELECT f.id, f.account_id, f.name, f.role, f.sort_order,
               f.total_messages, f.unread_messages
        FROM folder f
        JOIN mail_account a ON f.account_id = a.id
        WHERE a.user_id = ?
        ORDER BY f.sort_order, f.name
        ",
        |row| FolderResponse {
            id: row.get("id"),
            account_id: row.get("account_id"),
            name: row.get("name"),
            role: row.get("role"),
            sort_order: row.get("sort_order"),
            total_messages: row.get("total_messages"),
            unread_messages: row.get("unread_messages"),
        },
        &user_id
    )?;

    Ok(Json(folders))
}

/// List messages in a folder.
async fn list_messages(
    State(state): State<AuthState>,
    Path(folder_id): Path<String>,
    AuthUser(user_id): AuthUser,
) -> Result<Json<Vec<MessageResponse>>, SyncError> {
    let db = state.db();

    // Verify folder belongs to the user
    let check: Option<String> = db_scalar_optional!(
        db,
        String,
        r"
        SELECT f.id FROM folder f
        JOIN mail_account a ON f.account_id = a.id
        WHERE f.id = ? AND a.user_id = ?
        ",
        &folder_id,
        &user_id
    )?;
    if check.is_none() {
        return Err(SyncError::AccountNotFound);
    }

    let messages = db_fetch_all!(
        db,
        r"
        SELECT id, account_id, folder_id, subject, from_address,
               to_addresses, cc_addresses, date, snippet,
               body_text, body_html, is_read, is_starred, has_attachments
        FROM message
        WHERE folder_id = ? AND is_deleted = 0
          AND (snoozed_until IS NULL OR snoozed_until <= datetime('now'))
        ORDER BY date DESC
        LIMIT 500
        ",
        |row| message_response_from_sql!(row),
        &folder_id
    )?;

    Ok(Json(messages))
}

/// Query for GET /api/v1/messages (unified inbox / role filter).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListMessagesQuery {
    role: Option<String>,
    account_id: Option<String>,
}

/// List messages across folders, optionally filtered by standard role and account.
async fn list_messages_query(
    State(state): State<AuthState>,
    AuthUser(user_id): AuthUser,
    Query(query): Query<ListMessagesQuery>,
) -> Result<Json<Vec<MessageResponse>>, SyncError> {
    let db = state.db();
    let messages = query_user_messages(
        db,
        &user_id,
        query.role.as_deref(),
        query.account_id.as_deref(),
    )
    .await?;
    Ok(Json(messages))
}

/// List messages for a user, optionally filtered by folder role and account.
async fn query_user_messages(
    db: &DbPool,
    user_id: &str,
    role: Option<&str>,
    account_id: Option<&str>,
) -> Result<Vec<MessageResponse>, SyncError> {
    db_fetch_all!(
        db,
        r"
        SELECT m.id, m.account_id, m.folder_id, m.subject, m.from_address,
               m.to_addresses, m.cc_addresses, m.date, m.snippet,
               m.body_text, m.body_html, m.is_read, m.is_starred, m.has_attachments
        FROM message m
        JOIN folder f ON m.folder_id = f.id
        JOIN mail_account a ON m.account_id = a.id
        WHERE a.user_id = ?
          AND m.is_deleted = 0
          AND (m.snoozed_until IS NULL OR m.snoozed_until <= datetime('now'))
          AND (? IS NULL OR f.role = ?)
          AND (? IS NULL OR m.account_id = ?)
        ORDER BY m.date DESC
        LIMIT 500
        ",
        |row| message_response_from_sql!(row),
        user_id,
        role,
        role,
        account_id,
        account_id
    )
    .map_err(SyncError::from)
}

/// Query for GET /api/v1/messages/search.
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
    AuthUser(user_id): AuthUser,
    Query(query): Query<SearchMessagesQuery>,
) -> Result<Json<Vec<MessageResponse>>, SyncError> {
    let db = state.db();

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

    let messages = db_fetch_all!(
        db,
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
        |row| message_response_from_sql!(row),
        &user_id,
        &query.account_id,
        &query.account_id,
        &query.folder_id,
        &query.folder_id,
        &pattern,
        &pattern,
        &pattern,
        &pattern,
        limit
    )?;

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
    AuthUser(user_id): AuthUser,
) -> Result<Json<Vec<AttachmentResponse>>, SyncError> {
    let db = state.db();
    let _ = load_message_row(db, &user_id, &message_id).await?;

    let rows = db_fetch_all!(
        db,
        r"
        SELECT id, message_id, filename, content_type, size_bytes, is_inline
        FROM attachment
        WHERE message_id = ?
        ORDER BY created_at ASC
        ",
        |row| AttachmentResponse {
            id: row.get("id"),
            message_id: row.get("message_id"),
            filename: row.get("filename"),
            content_type: row.get("content_type"),
            size_bytes: row.get("size_bytes"),
            is_inline: row.get("is_inline"),
        },
        &message_id
    )?;

    Ok(Json(rows))
}

async fn download_attachment(
    State(state): State<AuthState>,
    Path(attachment_id): Path<String>,
    AuthUser(user_id): AuthUser,
) -> Result<Response, SyncError> {
    let db = state.db();

    let row = db_fetch_optional!(
        db,
        r"
        SELECT a.id, a.filename, a.content_type, a.storage_path, a.message_id
        FROM attachment a
        JOIN message m ON a.message_id = m.id
        JOIN mail_account acc ON m.account_id = acc.id
        WHERE a.id = ? AND acc.user_id = ?
        ",
        |row| {
            let storage_path: String = row.get("storage_path");
            let filename: Option<String> = row.get("filename");
            let content_type: Option<String> = row.get("content_type");
            (storage_path, filename, content_type)
        },
        &attachment_id,
        &user_id
    )?
    .ok_or(SyncError::MessageNotFound)?;

    let (storage_path, filename, content_type) = row;

    let bytes = tokio::fs::read(&storage_path).await.map_err(|e| {
        tracing::error!(error = %e, path = %storage_path, "attachment missing on disk");
        SyncError::InvalidInput("attachment not available".to_string())
    })?;

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
    db: &DbPool,
    data_dir: &std::path::Path,
    message_id: &str,
    attachments: &[crate::imap::ExtractedAttachment],
) -> Result<(), SyncError> {
    if attachments.is_empty() {
        return Ok(());
    }

    // Replace prior attachment rows for this message (re-fetch).
    db_execute!(
        db,
        "DELETE FROM attachment WHERE message_id = ?",
        message_id
    )?;

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
        db_execute!(
            db,
            r"
            INSERT INTO attachment (
                id, message_id, filename, content_type, size_bytes,
                storage_path, content_id, is_inline
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)
            ",
            &id,
            message_id,
            &att.filename,
            &att.content_type,
            size,
            path.to_string_lossy().as_ref(),
            &att.content_id,
            att.is_inline
        )?;
    }

    db_execute!(
        db,
        "UPDATE message SET has_attachments = 1, updated_at = datetime('now') WHERE id = ?",
        message_id
    )?;

    Ok(())
}

/// Request body for PATCH /api/v1/messages/{id}.
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
    db: &DbPool,
    user_id: &str,
    message_id: &str,
) -> Result<MessageRow, SyncError> {
    db_fetch_optional!(
        db,
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
        |row| MessageRow {
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
        },
        message_id,
        user_id
    )?
    .ok_or(SyncError::MessageNotFound)
}

async fn connect_imap_for_account(
    db: &DbPool,
    user_id: &str,
    account_id: &str,
) -> Result<(ImapClient, String), SyncError> {
    let row = db_fetch_optional!(
        db,
        r"
        SELECT email_address, credential, imap_host, imap_port, imap_security, protocol
        FROM mail_account
        WHERE id = ? AND user_id = ? AND is_active = 1
        ",
        |row| {
            let protocol: String = row.get("protocol");
            let imap_host: Option<String> = row.get("imap_host");
            let imap_port: Option<i32> = row.get("imap_port");
            let imap_security: Option<String> = row.get("imap_security");
            let email_address: String = row.get("email_address");
            let credential_json: String = row.get("credential");
            (
                protocol,
                imap_host,
                imap_port,
                imap_security,
                email_address,
                credential_json,
            )
        },
        account_id,
        user_id
    )?
    .ok_or(SyncError::AccountNotFound)?;

    let (protocol, imap_host, imap_port, imap_security, email_address, credential_json) = row;
    if protocol != "imap" {
        return Err(SyncError::InvalidInput(
            "remote flag/move ops require an IMAP account in v1".into(),
        ));
    }

    let host =
        imap_host.ok_or_else(|| SyncError::InvalidInput("IMAP host not configured".into()))?;
    let port = u16::try_from(imap_port.unwrap_or(993)).unwrap_or(993);
    let security = match imap_security.as_deref() {
        Some(s) => {
            match crate::netsec::normalize_security_mode(s).map_err(SyncError::InvalidInput)? {
                "starttls" => ImapSecurity::Starttls,
                _ => ImapSecurity::Tls,
            }
        }
        None => ImapSecurity::Tls,
    };

    let dek = crate::auth::AuthState::get_user_dek(db, user_id)
        .await
        .map_err(|e| SyncError::Crypto(e.to_string()))?;
    let password = crate::imap::decrypt_account_password(&credential_json, &dek)
        .map_err(|e| SyncError::Crypto(e.to_string()))?;

    let client = ImapClient::connect(&ImapConfig {
        host,
        port,
        security,
        username: email_address,
        password: zeroize::Zeroizing::new(password),
    })
    .await?;

    Ok((client, protocol))
}

fn parse_imap_uid(external_id: Option<&str>) -> Result<u32, SyncError> {
    external_id
        .and_then(|s| s.parse::<u32>().ok())
        .ok_or_else(|| SyncError::InvalidInput("message has no IMAP UID".into()))
}

/// GET /api/v1/messages/{id} — return one message; lazily fetch IMAP body if missing.
async fn get_message(
    State(state): State<AuthState>,
    Path(message_id): Path<String>,
    AuthUser(user_id): AuthUser,
) -> Result<Json<MessageResponse>, SyncError> {
    let db = state.db();
    let mut row = load_message_row(db, &user_id, &message_id).await?;

    let needs_body = row.body_text.is_none() && row.body_html.is_none();
    if needs_body
        && row.protocol == "imap"
        && let Ok(uid) = parse_imap_uid(row.external_id.as_deref())
    {
        let (mut client, _) = connect_imap_for_account(db, &user_id, &row.account_id).await?;
        client.select(&row.folder_name).await?;
        let bodies = client.fetch_bodies(&[uid]).await?;
        if let Some(fetched) = bodies.into_iter().next() {
            db_execute!(
                db,
                r"
                    UPDATE message
                    SET body_text = ?, body_html = ?, has_attachments = ?,
                        snippet = COALESCE(snippet, ?),
                        updated_at = datetime('now')
                    WHERE id = ?
                    ",
                &fetched.body_text,
                &fetched.body_html,
                fetched.has_attachments,
                fetched
                    .body_text
                    .as_deref()
                    .map(|t| t.chars().take(120).collect::<String>()),
                &row.id
            )?;

            row.body_text = fetched.body_text;
            row.body_html = fetched.body_html;
            row.has_attachments = fetched.has_attachments || !fetched.attachments.is_empty();
            persist_attachments(db, &state.data_dir, &row.id, &fetched.attachments).await?;
        }
    }

    Ok(Json(message_response_from_row(&row)))
}

/// PATCH /api/v1/messages/{id} — update read/starred flags (IMAP STORE when possible).
async fn patch_message(
    State(state): State<AuthState>,
    Path(message_id): Path<String>,
    AuthUser(user_id): AuthUser,
    Json(body): Json<PatchMessageRequest>,
) -> Result<Json<MessageResponse>, SyncError> {
    let db = state.db();
    let mut row = load_message_row(db, &user_id, &message_id).await?;

    if body.is_read.is_none() && body.is_starred.is_none() {
        return Err(SyncError::InvalidInput(
            "provide isRead and/or isStarred".into(),
        ));
    }

    let next_read = body.is_read.unwrap_or(row.is_read);
    let next_star = body.is_starred.unwrap_or(row.is_starred);

    if row.protocol == "imap" && (body.is_read.is_some() || body.is_starred.is_some()) {
        let uid = parse_imap_uid(row.external_id.as_deref())?;
        let (mut client, _) = connect_imap_for_account(db, &user_id, &row.account_id).await?;
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

    db_execute!(
        db,
        r"
        UPDATE message
        SET is_read = ?, is_starred = ?, updated_at = datetime('now')
        WHERE id = ?
        ",
        next_read,
        next_star,
        &row.id
    )?;

    row.is_read = next_read;
    row.is_starred = next_star;

    // Keep folder unread counts roughly consistent.
    update_folder_counts(db, &row.folder_id).await?;

    Ok(Json(message_response_from_row(&row)))
}

/// POST /api/v1/messages/{id}/trash — move to Trash (IMAP) or soft-delete locally.
async fn trash_message(
    State(state): State<AuthState>,
    Path(message_id): Path<String>,
    AuthUser(user_id): AuthUser,
) -> Result<Json<serde_json::Value>, SyncError> {
    move_message_to_role(state, message_id, user_id, "trash").await
}

/// POST /api/v1/messages/{id}/archive — move to Archive when available.
async fn archive_message(
    State(state): State<AuthState>,
    Path(message_id): Path<String>,
    AuthUser(user_id): AuthUser,
) -> Result<Json<serde_json::Value>, SyncError> {
    move_message_to_role(state, message_id, user_id, "archive").await
}

/// POST /api/v1/messages/{id}/snooze — hide locally until `until`, then unsnooze via job.
#[derive(Deserialize)]
struct SnoozeRequest {
    until: String,
}

/// UTC instant as SQLite `datetime()` text (`YYYY-MM-DD HH:MM:SS`).
///
/// Inbox/folder filters compare `snoozed_until <= datetime('now')`. RFC3339 with a `T`
/// separator sorts after the same wall time with a space, so same-day overdue rows would
/// stay hidden if we stored client RFC3339 literally.
fn sqlite_utc_datetime(dt: chrono::DateTime<chrono::Utc>) -> String {
    dt.format("%Y-%m-%d %H:%M:%S").to_string()
}

async fn snooze_message(
    State(state): State<AuthState>,
    Path(message_id): Path<String>,
    AuthUser(user_id): AuthUser,
    Json(body): Json<SnoozeRequest>,
) -> Result<Json<serde_json::Value>, SyncError> {
    let db = state.db();
    let row = load_message_row(db, &user_id, &message_id).await?;

    let until_utc = chrono::DateTime::parse_from_rfc3339(&body.until)
        .map_err(|_| SyncError::InvalidInput("until must be RFC3339".into()))?
        .to_utc();
    let until_api = until_utc.to_rfc3339();
    let until_sql = sqlite_utc_datetime(until_utc);

    db_execute!(
        db,
        r"
        UPDATE message
        SET snoozed_until = ?, updated_at = datetime('now')
        WHERE id = ?
        ",
        &until_sql,
        &row.id
    )?;

    // Job claim compares `run_at` to RFC3339 `now` from chrono — keep that format.
    crate::jobs::enqueue(
        db,
        &crate::jobs::JobPayload::UnsnoozeMessage {
            message_id: row.id.clone(),
        },
        &until_api,
    )
    .await?;

    Ok(Json(serde_json::json!({
        "status": "ok",
        "until": until_api,
    })))
}

async fn move_message_to_role(
    state: AuthState,
    message_id: String,
    user_id: String,
    role: &str,
) -> Result<Json<serde_json::Value>, SyncError> {
    let db = state.db();
    let row = load_message_row(db, &user_id, &message_id).await?;

    let dest = db_fetch_optional!(
        db,
        r"
        SELECT id, external_id, name FROM folder
        WHERE account_id = ? AND role = ?
        ORDER BY sort_order ASC
        LIMIT 1
        ",
        |dest| {
            let dest_id: String = dest.get("id");
            let dest_name: String = dest
                .get::<Option<String>, _>("external_id")
                .unwrap_or_else(|| dest.get("name"));
            (dest_id, dest_name)
        },
        &row.account_id,
        role
    )?;

    let Some((dest_id, dest_name)) = dest else {
        // No destination folder: soft-delete locally.
        db_execute!(
            db,
            "UPDATE message SET is_deleted = 1, updated_at = datetime('now') WHERE id = ?",
            &row.id
        )?;
        update_folder_counts(db, &row.folder_id).await?;
        return Ok(Json(serde_json::json!({
            "status": "ok",
            "action": "soft_delete",
            "role": role,
        })));
    };

    if row.protocol == "imap" {
        let uid = parse_imap_uid(row.external_id.as_deref())?;
        let (mut client, _) = connect_imap_for_account(db, &user_id, &row.account_id).await?;
        client.select(&row.folder_name).await?;
        client.move_uid(uid, &dest_name).await?;
    }

    db_execute!(
        db,
        r"
        UPDATE message
        SET folder_id = ?, updated_at = datetime('now')
        WHERE id = ?
        ",
        &dest_id,
        &row.id
    )?;

    update_folder_counts(db, &row.folder_id).await?;
    update_folder_counts(db, &dest_id).await?;

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
    let row = db_fetch_optional!(
        db,
        r"
        SELECT receive_protocol, protocol, is_active, sync_enabled
        FROM mail_account
        WHERE id = ? AND user_id = ?
        ",
        |row| {
            let is_active: bool = row.get("is_active");
            let sync_enabled: bool = row.get("sync_enabled");
            let receive_protocol: Option<String> = row.get("receive_protocol");
            let protocol: String = row.get("protocol");
            (is_active, sync_enabled, receive_protocol, protocol)
        },
        account_id,
        user_id
    )?
    .ok_or(SyncError::AccountNotFound)?;

    let (is_active, sync_enabled, receive_protocol, protocol) = row;

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

    db_execute!(
        db,
        "UPDATE mail_account SET last_sync_at = datetime('now'), updated_at = datetime('now') WHERE id = ?",
        account_id
    )?;

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
    let row = load_account_sync_row(db, user_id, account_id).await?;
    let credential_json = row.credential.clone();
    let dek = crate::auth::AuthState::get_user_dek(db, user_id)
        .await
        .map_err(|e| SyncError::Crypto(e.to_string()))?;
    let password = crate::imap::decrypt_account_password(&credential_json, &dek)?;
    let result = run_imap_sync(db, account_id, &row, &password).await?;
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
    let row = load_account_sync_row(db, user_id, account_id).await?;
    let credential_json = row.credential.clone();
    let email_address = row.email_address.clone();
    let jmap_base_url = row.jmap_base_url.clone();
    let dek = crate::auth::AuthState::get_user_dek(db, user_id)
        .await
        .map_err(|e| SyncError::Crypto(e.to_string()))?;

    let result = if let Some(ref base_url) = jmap_base_url {
        let password = crate::jmap::decrypt_account_password(&credential_json, &dek)?;
        match run_jmap_sync(db, account_id, base_url, &email_address, &password).await {
            Ok(result) => result,
            Err(e) => {
                tracing::warn!("JMAP sync failed ({e}), falling back to IMAP");
                let password = crate::imap::decrypt_account_password(&credential_json, &dek)?;
                run_imap_sync(db, account_id, &row, &password).await?
            }
        }
    } else {
        let password = crate::imap::decrypt_account_password(&credential_json, &dek)?;
        run_imap_sync(db, account_id, &row, &password).await?
    };
    Ok(outcome_from_response(&result))
}

pub(crate) struct AccountSyncRow {
    email_address: String,
    credential: String,
    imap_host: Option<String>,
    imap_port: Option<i32>,
    imap_security: Option<String>,
    jmap_base_url: Option<String>,
}

async fn load_account_sync_row(
    db: &DbPool,
    user_id: &str,
    account_id: &str,
) -> Result<AccountSyncRow, SyncError> {
    db_fetch_optional!(
        db,
        r"
        SELECT id, email_address, protocol, credential,
               imap_host, imap_port, imap_security,
               jmap_base_url,
               is_active, sync_enabled
        FROM mail_account
        WHERE id = ? AND user_id = ?
        ",
        |row| AccountSyncRow {
            email_address: row.get("email_address"),
            credential: row.get("credential"),
            imap_host: row.get("imap_host"),
            imap_port: row.get("imap_port"),
            imap_security: row.get("imap_security"),
            jmap_base_url: row.get("jmap_base_url"),
        },
        account_id,
        user_id
    )?
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
    db: &DbPool,
    account_id: &str,
    row: &AccountSyncRow,
    password: &str,
) -> Result<SyncResponse, SyncError> {
    let imap_host = row.imap_host.clone();
    let imap_port = row.imap_port;
    let imap_security = row.imap_security.clone();
    let email_address = row.email_address.clone();

    let host = imap_host.ok_or_else(|| SyncError::Crypto("IMAP host not configured".into()))?;
    let port = u16::try_from(imap_port.unwrap_or(993)).unwrap_or(993);
    let security = match imap_security.as_deref() {
        Some(s) => {
            match crate::netsec::normalize_security_mode(s).map_err(SyncError::InvalidInput)? {
                "starttls" => ImapSecurity::Starttls,
                _ => ImapSecurity::Tls,
            }
        }
        None => ImapSecurity::Tls,
    };

    let config = ImapConfig {
        host,
        port,
        security,
        username: email_address,
        password: zeroize::Zeroizing::new(password.to_string()),
    };

    // Connect to IMAP
    let mut client = ImapClient::connect(&config).await?;

    // Sync folders
    let folders = client.list_folders().await?;
    let mut folders_synced = 0;

    for folder in &folders {
        upsert_folder(db, account_id, &folder.name, folder.delimiter.as_deref()).await?;
        folders_synced += 1;
    }

    // Sync messages in each folder
    let mut total_new = 0;
    let mut total_updated = 0;
    let total_deleted = 0;

    for folder in &folders {
        let folder_id = get_folder_id(db, account_id, &folder.name).await?;

        let (uid_validity, _uid_next, _exists) = client.select(&folder.name).await?;

        // Load existing cursor
        let cursor = load_cursor(db, account_id, &folder_id).await?;

        // Check if UIDVALIDITY changed — if so, we need a full resync
        let after_uid = if let Some(ref c) = cursor {
            if c.uid_validity == uid_validity {
                Some(c.last_uid)
            } else {
                // UIDVALIDITY changed: clear old messages for this folder and resync
                clear_folder_messages(db, &folder_id).await?;
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
            let was_new = upsert_message(db, account_id, &folder_id, msg).await?;
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
            db,
            account_id,
            &folder_id,
            "imap",
            uid_validity,
            new_cursor_uid,
        )
        .await?;

        // Update folder counts
        update_folder_counts(db, &folder_id).await?;
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
    db: &DbPool,
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
        upsert_folder(db, account_id, &mb.name, mb.role.as_deref()).await?;
        folders_synced += 1;
    }

    // 3. Sync emails per mailbox
    let mut total_new = 0;
    let mut total_updated = 0;

    for mb in &mailboxes {
        let folder_id = get_folder_id(db, account_id, &mb.name).await?;

        // Load stored JMAP state (raw queryState token, sent back as sinceState)
        let since_state = load_jmap_cursor(db, account_id, &folder_id).await?;

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
            let was_new = upsert_jmap_message(db, account_id, &folder_id, email_obj).await?;
            if was_new {
                total_new += 1;
            } else {
                total_updated += 1;
            }
        }

        // Save JMAP cursor: store the raw queryState token verbatim
        if let Some(ref qs) = query_result.query_state {
            save_jmap_cursor(db, account_id, &folder_id, qs).await?;
        }

        update_folder_counts(db, &folder_id).await?;
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
    db: &DbPool,
    account_id: &str,
    folder_id: &str,
    email: &crate::jmap::JmapEmail,
) -> Result<bool, SyncError> {
    let external_id = &email.id;

    let existing: Option<String> = db_scalar_optional!(
        db,
        String,
        "SELECT id FROM message WHERE account_id = ? AND external_id = ?",
        account_id,
        external_id
    )?;

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
        db_execute!(
            db,
            r"
            UPDATE message SET
                is_read = ?,
                is_starred = ?,
                flags = ?,
                updated_at = datetime('now')
            WHERE id = ?
            ",
            is_read,
            is_starred,
            &flags_json,
            &id
        )?;

        Ok(false)
    } else {
        let id = uuid::Uuid::new_v7(uuid::Timestamp::now(uuid::NoContext)).to_string();

        db_execute!(
            db,
            r"
            INSERT INTO message (
                id, account_id, folder_id, external_id,
                message_id_header, subject, from_address, to_addresses,
                cc_addresses, date, is_read, is_starred,
                flags, size_bytes, in_reply_to, references_headers,
                snippet, has_attachments, body_text, body_html
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            ",
            &id,
            account_id,
            folder_id,
            external_id,
            email.message_id_header(),
            &email.subject,
            &from_json,
            &to_json,
            &cc_json,
            &email.received_at,
            is_read,
            is_starred,
            &flags_json,
            email.size.map(|s| i32::try_from(s).unwrap_or(i32::MAX)),
            email
                .in_reply_to
                .as_ref()
                .and_then(|ids| ids.first())
                .cloned(),
            email.references.as_ref().map(|refs| refs.join(" ")),
            &snippet,
            email.has_attachment.unwrap_or(false),
            email.body_text(),
            email.body_html()
        )?;

        Ok(true)
    }
}

// ── Database helpers ────────────────────────────────────────────────

/// Upsert a folder by `(account_id, name)`.
///
/// Returns `Ok(())` on success. Uses the `external_id` field to store
/// the IMAP folder name for uniqueness.
async fn upsert_folder(
    db: &DbPool,
    account_id: &str,
    name: &str,
    _delimiter: Option<&str>,
) -> Result<(), SyncError> {
    let role = infer_folder_role(name);
    let external_id = name;

    // Try to find existing folder
    let existing: Option<String> = db_scalar_optional!(
        db,
        String,
        "SELECT id FROM folder WHERE account_id = ? AND external_id = ?",
        account_id,
        external_id
    )?;

    if let Some(id) = existing {
        db_execute!(
            db,
            "UPDATE folder SET name = ?, updated_at = datetime('now') WHERE id = ?",
            name,
            &id
        )?;
    } else {
        let id = Uuid::new_v7(uuid::Timestamp::now(uuid::NoContext)).to_string();
        db_execute!(
            db,
            r"
            INSERT INTO folder (id, account_id, external_id, name, role, sort_order)
            VALUES (?, ?, ?, ?, ?, 0)
            ",
            &id,
            account_id,
            external_id,
            name,
            role
        )?;
    }

    Ok(())
}

/// Get the local folder ID by `external_id`.
async fn get_folder_id(db: &DbPool, account_id: &str, name: &str) -> Result<String, SyncError> {
    db_scalar_optional!(
        db,
        String,
        "SELECT id FROM folder WHERE account_id = ? AND external_id = ?",
        account_id,
        name
    )?
    .ok_or_else(|| SyncError::Database(sqlx::Error::RowNotFound))
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
    db: &DbPool,
    account_id: &str,
    folder_id: &str,
) -> Result<Option<SyncCursorInfo>, SyncError> {
    let value: Option<String> = db_scalar_optional!(
        db,
        String,
        r"
        SELECT cursor_value
        FROM sync_cursor
        WHERE account_id = ? AND folder_id = ? AND cursor_type = 'uidvalidity_uid'
        ",
        account_id,
        folder_id
    )?;
    Ok(value.as_deref().map(parse_cursor_value))
}

/// Save the sync cursor for a folder.
///
/// Cursor format: `{uid_validity}:{last_uid}` for the `uidvalidity_uid` type.
async fn save_cursor(
    db: &DbPool,
    account_id: &str,
    folder_id: &str,
    protocol: &str,
    uid_validity: u32,
    last_uid: u32,
) -> Result<(), SyncError> {
    let cursor_value = format!("{uid_validity}:{last_uid}");
    let cursor_id = format!("{account_id}:{folder_id}:uidvalidity_uid");

    db_execute!(
        db,
        r"
        INSERT INTO sync_cursor (id, account_id, folder_id, protocol, cursor_type, cursor_value, updated_at)
        VALUES (?, ?, ?, ?, 'uidvalidity_uid', ?, datetime('now'))
        ON CONFLICT(account_id, folder_id, cursor_type)
        DO UPDATE SET cursor_value = excluded.cursor_value, updated_at = excluded.updated_at
        ",
        &cursor_id,
        account_id,
        folder_id,
        protocol,
        &cursor_value
    )?;

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

/// Load the stored JMAP `queryState` token for a folder.
///
/// Returns the raw token to be sent verbatim as `sinceState` on the next sync.
async fn load_jmap_cursor(
    db: &DbPool,
    account_id: &str,
    folder_id: &str,
) -> Result<Option<String>, SyncError> {
    db_scalar_optional!(
        db,
        String,
        r"
        SELECT cursor_value
        FROM sync_cursor
        WHERE account_id = ? AND folder_id = ? AND cursor_type = 'state_token'
        ",
        account_id,
        folder_id
    )
    .map_err(SyncError::from)
}

/// Save the JMAP `queryState` token for a folder.
///
/// The token is an opaque server string and is stored verbatim — never hashed —
/// so it can be sent back as `sinceState` on the next sync.
async fn save_jmap_cursor(
    db: &DbPool,
    account_id: &str,
    folder_id: &str,
    query_state: &str,
) -> Result<(), SyncError> {
    let cursor_id = format!("{account_id}:{folder_id}:state_token");

    db_execute!(
        db,
        r"
        INSERT INTO sync_cursor (id, account_id, folder_id, protocol, cursor_type, cursor_value, updated_at)
        VALUES (?, ?, ?, 'jmap', 'state_token', ?, datetime('now'))
        ON CONFLICT(account_id, folder_id, cursor_type)
        DO UPDATE SET cursor_value = excluded.cursor_value, updated_at = excluded.updated_at
        ",
        &cursor_id,
        account_id,
        folder_id,
        query_state
    )?;

    Ok(())
}

// ── Message upsert ──────────────────────────────────────────────────

/// Upsert a message from IMAP metadata.
///
/// Returns `true` if the message was newly inserted, `false` if updated.
async fn upsert_message(
    db: &DbPool,
    account_id: &str,
    folder_id: &str,
    msg: &ImapMessage,
) -> Result<bool, SyncError> {
    let external_id = msg.uid.to_string();

    // Check if message already exists
    let existing: Option<String> = db_scalar_optional!(
        db,
        String,
        "SELECT id FROM message WHERE account_id = ? AND external_id = ?",
        account_id,
        &external_id
    )?;

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

    let was_new = existing.is_none();
    let id =
        existing.unwrap_or_else(|| Uuid::new_v7(uuid::Timestamp::now(uuid::NoContext)).to_string());

    // INSERT snoozed_until as NULL; ON CONFLICT must not overwrite an existing snooze.
    db_execute!(
        db,
        r"
        INSERT INTO message (
            id, account_id, folder_id, external_id,
            message_id_header, subject, from_address, to_addresses,
            cc_addresses, date, is_read, is_starred,
            flags, size_bytes, in_reply_to, references_headers,
            snippet, has_attachments, body_text, body_html, snoozed_until
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, NULL)
        ON CONFLICT(account_id, external_id) DO UPDATE SET
            is_read = excluded.is_read,
            is_starred = excluded.is_starred,
            flags = excluded.flags,
            updated_at = datetime('now')
        ",
        &id,
        account_id,
        folder_id,
        &external_id,
        &msg.message_id,
        &msg.subject,
        &from_json,
        &to_json,
        &msg.cc,
        &msg.date,
        is_read,
        is_starred,
        &flags_json,
        msg.size.map(i32::try_from).transpose().unwrap_or(None),
        &msg.in_reply_to,
        &msg.references,
        &snippet,
        msg.has_attachments,
        &msg.body_text,
        &msg.body_html
    )?;

    Ok(was_new)
}

/// Delete all messages in a folder (used when UIDVALIDITY changes).
async fn clear_folder_messages(db: &DbPool, folder_id: &str) -> Result<(), SyncError> {
    db_execute!(db, "DELETE FROM message WHERE folder_id = ?", folder_id)?;
    db_execute!(db, "DELETE FROM sync_cursor WHERE folder_id = ?", folder_id)?;
    Ok(())
}

/// Update folder message counts from the message table.
async fn update_folder_counts(db: &DbPool, folder_id: &str) -> Result<(), SyncError> {
    let total: i64 = db_scalar!(
        db,
        i64,
        "SELECT COUNT(*) FROM message WHERE folder_id = ? AND is_deleted = 0",
        folder_id
    )?;

    let unread: i64 = db_scalar!(
        db,
        i64,
        "SELECT COUNT(*) FROM message WHERE folder_id = ? AND is_deleted = 0 AND is_read = 0",
        folder_id
    )?;

    db_execute!(
        db,
        "UPDATE folder SET total_messages = ?, unread_messages = ?, updated_at = datetime('now') WHERE id = ?",
        i32::try_from(total).unwrap_or(i32::MAX),
        i32::try_from(unread).unwrap_or(i32::MAX),
        folder_id
    )?;

    Ok(())
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::Storage;

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
            redis_url: None,
            master_key: crate::auth::TEST_MASTER_KEY.to_vec(),
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
        upsert_folder(&as_db(&pool), &account_id, "INBOX", Some("/"))
            .await
            .unwrap();

        let id1 = get_folder_id(&as_db(&pool), &account_id, "INBOX")
            .await
            .unwrap();

        // Second upsert (update) — should not create a new row
        upsert_folder(&as_db(&pool), &account_id, "INBOX", Some("/"))
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
    async fn upsert_message_idempotent() {
        let pool = test_pool().await;
        let (_, account_id) = seed_user_and_account(&pool).await;

        // Create a folder first
        upsert_folder(&as_db(&pool), &account_id, "INBOX", Some("/"))
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

        upsert_folder(&as_db(&pool), &account_id, "INBOX", Some("/"))
            .await
            .unwrap();
        let folder_id = get_folder_id(&as_db(&pool), &account_id, "INBOX")
            .await
            .unwrap();

        // Save cursor
        save_cursor(&as_db(&pool), &account_id, &folder_id, "imap", 12345, 100)
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
        save_cursor(&as_db(&pool), &account_id, &folder_id, "imap", 12345, 200)
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

        upsert_folder(&as_db(&pool), &account_id, "INBOX", Some("/"))
            .await
            .unwrap();
        let folder_id = get_folder_id(&as_db(&pool), &account_id, "INBOX")
            .await
            .unwrap();

        // A JMAP queryState is an opaque server token — it must round-trip verbatim,
        // because it is sent back to the server as `sinceState` on the next sync.
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

        // What we send back as sinceState must equal the original token.
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
    }

    #[tokio::test]
    async fn clear_folder_messages_works() {
        let pool = test_pool().await;
        let (_, account_id) = seed_user_and_account(&pool).await;

        upsert_folder(&as_db(&pool), &account_id, "INBOX", Some("/"))
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

        // Save cursor
        save_cursor(&as_db(&pool), &account_id, &folder_id, "imap", 100, 1)
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

        upsert_folder(&as_db(&pool), &account_id, "INBOX", Some("/"))
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

    async fn message_id_for_uid(pool: &sqlx::SqlitePool, account_id: &str, uid: u32) -> String {
        sqlx::query_scalar("SELECT id FROM message WHERE account_id = ? AND external_id = ?")
            .bind(account_id)
            .bind(uid.to_string())
            .fetch_one(pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn inbox_hides_snoozed_message() {
        let pool = test_pool().await;
        let (user_id, account_id) = seed_user_and_account(&pool).await;
        upsert_folder(&as_db(&pool), &account_id, "INBOX", Some("/"))
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
        let message_id = message_id_for_uid(&pool, &account_id, 7).await;
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
        upsert_folder(&as_db(&pool), &account_id, "INBOX", Some("/"))
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
        let message_id = message_id_for_uid(&pool, &account_id, 10).await;

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
    async fn unsnooze_job_clears_column() {
        let pool = test_pool().await;
        let (user_id, account_id) = seed_user_and_account(&pool).await;
        upsert_folder(&as_db(&pool), &account_id, "INBOX", Some("/"))
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
        let message_id = message_id_for_uid(&pool, &account_id, 8).await;
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
        upsert_folder(&as_db(&pool), &account_id, "INBOX", Some("/"))
            .await
            .unwrap();
        let folder_id = get_folder_id(&as_db(&pool), &account_id, "INBOX")
            .await
            .unwrap();
        let msg = sample_imap_message(9);
        upsert_message(&as_db(&pool), &account_id, &folder_id, &msg)
            .await
            .unwrap();
        let message_id = message_id_for_uid(&pool, &account_id, 9).await;
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
        let body = std::str::from_utf8(&body).unwrap();
        assert_eq!(body, r#"{"error":"internal error"}"#);

        // Upstream protocol chatter (hostnames, usernames) masked as 502.
        let err = SyncError::Protocol("NO auth failed for bob@imap.example.com".into());
        let res = err.into_response();
        assert_eq!(res.status(), StatusCode::BAD_GATEWAY);
        let body = axum::body::to_bytes(res.into_body(), 4096).await.unwrap();
        let body = std::str::from_utf8(&body).unwrap();
        assert_eq!(body, r#"{"error":"internal error"}"#);
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
}
