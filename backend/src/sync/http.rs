//! Mail and sync HTTP handlers.

use axum::{
    Json, Router,
    body::Body,
    extract::{Path, Query, State},
    http::{HeaderValue, StatusCode, header},
    response::{
        Response,
        sse::{Event, KeepAlive, Sse},
    },
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use uuid::Uuid;

use super::send::send_message;
use super::store::update_folder_counts;
use super::types::{EnqueuedSync, SyncError, SyncStatus};
use crate::auth::{AuthState, AuthUser};
use crate::db_row::{
    TsParam, id_from_row, id_param, json_text_from_row, message_date_param, opt_id_from_row,
    opt_id_param, opt_json_param, opt_ts_from_row,
};
use crate::imap::{ImapClient, ImapConfig, ImapSecurity};
use crate::kernel::AppEvent;
use crate::privacy::{
    load_settings, rewrite_remote_images, sender_email_from_json, should_allow_remote,
};
use crate::sanitize::persist_body_html;
use crate::storage::DbPool;

pub fn routes() -> Router<AuthState> {
    Router::new()
        .route("/api/v1/sync/status", get(sync_status))
        .route("/api/v1/sync/events", get(sync_events))
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
pub(crate) async fn sync_status(
    State(state): State<AuthState>,
    AuthUser(user_id): AuthUser,
) -> Result<Json<SyncStatus>, SyncError> {
    let db = state.db();
    let user_bind = id_param(db, &user_id)?;

    let count: i64 = db_scalar!(
        db,
        i64,
        "SELECT COUNT(*) FROM mail_account WHERE user_id = ? AND is_active = ? AND sync_enabled = ?",
        &user_bind,
        true,
        true
    )?;

    // Pending or running sync_account jobs for this user (payload JSON has user_id).
    let syncing = user_has_active_sync_job(db, &user_id).await?;

    Ok(Json(SyncStatus {
        active_accounts: count,
        syncing,
    }))
}

pub(crate) fn sync_event_json(ev: &AppEvent) -> serde_json::Value {
    match ev {
        AppEvent::SyncStarted { account_id } => serde_json::json!({
            "type": "sync_started",
            "accountId": account_id,
        }),
        AppEvent::SyncComplete { account_id } => serde_json::json!({
            "type": "sync_complete",
            "accountId": account_id,
        }),
        AppEvent::SyncError { account_id, error } => serde_json::json!({
            "type": "sync_error",
            "accountId": account_id,
            "error": error,
        }),
    }
}

pub(crate) fn sse_from_app_event(ev: &AppEvent) -> Event {
    let body = sync_event_json(ev);
    let ty = body["type"].as_str().unwrap_or("message");
    Event::default().event(ty).data(body.to_string())
}

/// GET /api/v1/sync/events — SSE stream of sync lifecycle events.
pub(crate) async fn sync_events(
    State(state): State<AuthState>,
    AuthUser(_user_id): AuthUser,
) -> Sse<impl futures_util::Stream<Item = Result<Event, std::convert::Infallible>>> {
    let rx = state.app.events.subscribe();
    let stream = futures_util::stream::unfold(rx, |mut rx| async move {
        loop {
            match rx.recv().await {
                Ok(ev) => return Some((Ok(sse_from_app_event(&ev)), rx)),
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return None,
            }
        }
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}

/// True when any `sync_account` job for `user_id` is pending or running.
pub(crate) async fn user_has_active_sync_job(
    db: &DbPool,
    user_id: &str,
) -> Result<bool, SyncError> {
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
pub(crate) async fn trigger_sync(
    State(state): State<AuthState>,
    Path(account_id): Path<String>,
    AuthUser(user_id): AuthUser,
) -> Result<(StatusCode, Json<EnqueuedSync>), SyncError> {
    let db = state.db();
    let account_bind = id_param(db, &account_id)?;
    let user_bind = id_param(db, &user_id)?;

    let exists: Option<String> = db_id_optional!(
        db,
        "SELECT id FROM mail_account WHERE id = ? AND user_id = ?",
        &account_bind,
        &user_bind
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

/// Folder response for the API.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderResponse {
    pub id: String,
    pub account_id: String,
    pub name: String,
    pub role: Option<String>,
    pub parent_id: Option<String>,
    pub sort_order: i32,
    pub total_messages: i32,
    pub unread_messages: i32,
}

/// Message response for the API.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
#[allow(clippy::struct_excessive_bools)] // API DTO mirrors message flag columns.
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
    /// True when remote images were replaced with placeholders in this response.
    pub remote_content_blocked: bool,
}

macro_rules! message_response_from_sql {
    ($row:expr) => {{
        MessageResponse {
            id: id_from_row(&$row, "id"),
            account_id: id_from_row(&$row, "account_id"),
            folder_id: id_from_row(&$row, "folder_id"),
            subject: $row.get("subject"),
            from_address: json_text_from_row(&$row, "from_address"),
            to_addresses: json_text_from_row(&$row, "to_addresses"),
            cc_addresses: json_text_from_row(&$row, "cc_addresses"),
            date: opt_ts_from_row(&$row, "date"),
            snippet: $row.get("snippet"),
            body_text: $row.get("body_text"),
            body_html: $row.get("body_html"),
            is_read: $row.get::<bool, _>("is_read"),
            is_starred: $row.get::<bool, _>("is_starred"),
            has_attachments: $row.get::<bool, _>("has_attachments"),
            remote_content_blocked: false,
        }
    }};
}

/// List all folders for the authenticated user.
pub(crate) async fn list_folders(
    State(state): State<AuthState>,
    AuthUser(user_id): AuthUser,
) -> Result<Json<Vec<FolderResponse>>, SyncError> {
    let db = state.db();
    let user_bind = id_param(db, &user_id)?;

    let folders = db_fetch_all!(
        db,
        r"
        SELECT f.id, f.account_id, f.name, f.role, f.parent_id, f.sort_order,
               f.total_messages, f.unread_messages
        FROM folder f
        JOIN mail_account a ON f.account_id = a.id
        WHERE a.user_id = ?
        ORDER BY f.sort_order, f.name
        ",
        |row| FolderResponse {
            id: id_from_row(row, "id"),
            account_id: id_from_row(row, "account_id"),
            name: row.get("name"),
            role: row.get("role"),
            parent_id: opt_id_from_row(row, "parent_id"),
            sort_order: row.get("sort_order"),
            total_messages: row.get("total_messages"),
            unread_messages: row.get("unread_messages"),
        },
        &user_bind
    )?;

    Ok(Json(folders))
}

/// List messages in a folder.
pub(crate) async fn list_messages(
    State(state): State<AuthState>,
    Path(folder_id): Path<String>,
    AuthUser(user_id): AuthUser,
) -> Result<Json<Vec<MessageResponse>>, SyncError> {
    let db = state.db();
    let folder_bind = id_param(db, &folder_id)?;
    let user_bind = id_param(db, &user_id)?;

    // Verify folder belongs to the user
    let check: Option<String> = db_id_optional!(
        db,
        r"
        SELECT f.id FROM folder f
        JOIN mail_account a ON f.account_id = a.id
        WHERE f.id = ? AND a.user_id = ?
        ",
        &folder_bind,
        &user_bind
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
        WHERE folder_id = ? AND is_deleted = ?
          AND (snoozed_until IS NULL OR snoozed_until <= datetime('now'))
        ORDER BY date DESC
        LIMIT 500
        ",
        |row| message_response_from_sql!(row),
        &folder_bind,
        false
    )?;

    Ok(Json(messages))
}

/// Query for GET /api/v1/messages (unified inbox / role filter).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ListMessagesQuery {
    role: Option<String>,
    account_id: Option<String>,
}

/// List messages across folders, optionally filtered by standard role and account.
pub(crate) async fn list_messages_query(
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
pub(crate) async fn query_user_messages(
    db: &DbPool,
    user_id: &str,
    role: Option<&str>,
    account_id: Option<&str>,
) -> Result<Vec<MessageResponse>, SyncError> {
    let user_bind = id_param(db, user_id)?;
    let account_bind = opt_id_param(db, account_id)?;
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
          AND m.is_deleted = ?
          AND (m.snoozed_until IS NULL OR m.snoozed_until <= datetime('now'))
          AND (? IS NULL OR f.role = ?)
          AND (? IS NULL OR m.account_id = ?)
        ORDER BY m.date DESC
        LIMIT 500
        ",
        |row| message_response_from_sql!(row),
        &user_bind,
        false,
        role,
        role,
        &account_bind,
        &account_bind
    )
    .map_err(SyncError::from)
}

/// Query for GET /api/v1/messages/search.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SearchMessagesQuery {
    q: String,
    account_id: Option<String>,
    folder_id: Option<String>,
    limit: Option<i64>,
}

/// Search messages by subject / snippet / body / from (local index).
pub(crate) async fn search_messages(
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
    let user_bind = id_param(db, &user_id)?;
    let account_bind = opt_id_param(db, query.account_id.as_deref())?;
    let folder_bind = opt_id_param(db, query.folder_id.as_deref())?;

    let messages = db_fetch_all!(
        db,
        r"
        SELECT m.id, m.account_id, m.folder_id, m.subject, m.from_address,
               m.to_addresses, m.cc_addresses, m.date, m.snippet,
               m.body_text, m.body_html, m.is_read, m.is_starred, m.has_attachments
        FROM message m
        JOIN mail_account a ON m.account_id = a.id
        WHERE a.user_id = ?
          AND m.is_deleted = ?
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
        &user_bind,
        false,
        &account_bind,
        &account_bind,
        &folder_bind,
        &folder_bind,
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
pub(crate) struct AttachmentResponse {
    id: String,
    message_id: String,
    filename: Option<String>,
    content_type: Option<String>,
    size_bytes: Option<i64>,
    is_inline: bool,
}

pub(crate) async fn list_attachments(
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
            id: id_from_row(row, "id"),
            message_id: id_from_row(row, "message_id"),
            filename: row.get("filename"),
            content_type: row.get("content_type"),
            size_bytes: row.get("size_bytes"),
            is_inline: row.get("is_inline"),
        },
        &id_param(db, &message_id)?
    )?;

    Ok(Json(rows))
}

pub(crate) async fn download_attachment(
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
        &id_param(db, &attachment_id)?,
        &id_param(db, &user_id)?
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

pub(crate) async fn persist_attachments(
    db: &DbPool,
    data_dir: &std::path::Path,
    message_id: &str,
    attachments: &[crate::imap::ExtractedAttachment],
) -> Result<(), SyncError> {
    if attachments.is_empty() {
        return Ok(());
    }
    let message_bind = id_param(db, message_id)?;

    // Replace prior attachment rows for this message (re-fetch).
    db_execute!(
        db,
        "DELETE FROM attachment WHERE message_id = ?",
        &message_bind
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
            &id_param(db, &id)?,
            &message_bind,
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
        "UPDATE message SET has_attachments = ?, updated_at = datetime('now') WHERE id = ?",
        true,
        &message_bind
    )?;

    Ok(())
}

/// Request body for PATCH /api/v1/messages/{id}.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PatchMessageRequest {
    is_read: Option<bool>,
    is_starred: Option<bool>,
}

/// Loaded message row with account/folder context for mutations.
pub(crate) struct MessageRow {
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

pub(crate) fn message_response_from_row(row: &MessageRow) -> MessageResponse {
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
        remote_content_blocked: false,
    }
}

/// Apply remote-image policy at serve time (after storage sanitization).
pub(crate) async fn finalize_message_response(
    state: &AuthState,
    user_id: &str,
    row: &MessageRow,
    remote_content_allow: bool,
) -> Result<MessageResponse, SyncError> {
    let mut response = message_response_from_row(row);
    if response.body_html.is_none() {
        return Ok(response);
    }

    let settings = load_settings(state.kv(), user_id).await?;
    let sender = sender_email_from_json(row.from_address.as_deref());
    let allow = should_allow_remote(&settings, sender.as_deref(), remote_content_allow);

    if let Some(html) = response.body_html.as_mut() {
        let proxy_signer = if settings.remote_images == "proxy" && allow {
            Some(crate::media::ProxySigner::new(state.kv(), user_id).await?)
        } else {
            None
        };
        let rewritten = rewrite_remote_images(html, allow, proxy_signer.as_ref());
        response.remote_content_blocked = rewritten.blocked;
        *html = rewritten.html;
    }

    Ok(response)
}

pub(crate) async fn load_message_row(
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
        WHERE m.id = ? AND a.user_id = ? AND m.is_deleted = ?
        ",
        |row| MessageRow {
            id: id_from_row(&row, "id"),
            account_id: id_from_row(&row, "account_id"),
            folder_id: id_from_row(&row, "folder_id"),
            folder_name: row.get("folder_name"),
            external_id: row.get("external_id"),
            protocol: row.get("protocol"),
            body_text: row.get("body_text"),
            body_html: row.get("body_html"),
            is_read: row.get("is_read"),
            is_starred: row.get("is_starred"),
            subject: row.get("subject"),
            from_address: json_text_from_row(&row, "from_address"),
            to_addresses: json_text_from_row(&row, "to_addresses"),
            cc_addresses: json_text_from_row(&row, "cc_addresses"),
            date: opt_ts_from_row(&row, "date"),
            snippet: row.get("snippet"),
            has_attachments: row.get("has_attachments"),
        },
        &id_param(db, message_id)?,
        &id_param(db, user_id)?,
        false
    )?
    .ok_or(SyncError::MessageNotFound)
}

pub(crate) async fn connect_imap_for_account(
    db: &DbPool,
    user_id: &str,
    account_id: &str,
) -> Result<(ImapClient, String), SyncError> {
    let (dek, credential_json) =
        crate::auth::AuthState::get_user_dek_and_credential(db, user_id, account_id)
            .await
            .map_err(|e| SyncError::Crypto(e.to_string()))?;
    let row = db_fetch_optional!(
        db,
        r"
        SELECT email_address, imap_host, imap_port, imap_security, protocol
        FROM mail_account
        WHERE id = ? AND user_id = ? AND is_active = ?
        ",
        |row| {
            let protocol: String = row.get("protocol");
            let imap_host: Option<String> = row.get("imap_host");
            let imap_port: Option<i32> = row.get("imap_port");
            let imap_security: Option<String> = row.get("imap_security");
            let email_address: String = row.get("email_address");
            (protocol, imap_host, imap_port, imap_security, email_address)
        },
        &id_param(db, account_id)?,
        &id_param(db, user_id)?,
        true
    )?
    .ok_or(SyncError::AccountNotFound)?;

    let (protocol, imap_host, imap_port, imap_security, email_address) = row;
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

pub(crate) fn parse_imap_uid(external_id: Option<&str>) -> Result<u32, SyncError> {
    external_id
        .and_then(|s| s.parse::<u32>().ok())
        .ok_or_else(|| SyncError::InvalidInput("message has no IMAP UID".into()))
}

/// GET /api/v1/messages/{id} — return one message; lazily fetch IMAP body if missing.
#[derive(Debug, Deserialize)]
pub(crate) struct GetMessageQuery {
    #[serde(rename = "remote_content")]
    remote_content: Option<String>,
}

pub(crate) async fn get_message(
    State(state): State<AuthState>,
    Path(message_id): Path<String>,
    AuthUser(user_id): AuthUser,
    Query(query): Query<GetMessageQuery>,
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
            let from_json = fetched
                .from
                .as_ref()
                .map(|f| serde_json::json!({ "raw": f }).to_string());
            let to_json = fetched
                .to
                .as_ref()
                .map(|t| serde_json::json!(vec![t]).to_string());
            let snippet = fetched
                .subject
                .as_deref()
                .map(|s| {
                    if s.len() > 120 {
                        format!("{}...", &s[..117])
                    } else {
                        s.to_string()
                    }
                })
                .or_else(|| {
                    fetched
                        .body_text
                        .as_deref()
                        .map(|t| t.chars().take(120).collect())
                });
            let body_html = persist_body_html(fetched.body_html.as_deref());
            db_execute!(
                db,
                r"
                    UPDATE message
                    SET body_text = ?, body_html = ?, has_attachments = ?,
                        snippet = COALESCE(NULLIF(snippet, ''), ?),
                        subject = COALESCE(NULLIF(subject, ''), ?),
                        from_address = COALESCE(from_address, ?),
                        to_addresses = COALESCE(to_addresses, ?),
                        date = COALESCE(date, ?),
                        message_id_header = COALESCE(message_id_header, ?),
                        updated_at = datetime('now')
                    WHERE id = ?
                    ",
                &fetched.body_text,
                &body_html,
                fetched.has_attachments,
                &snippet,
                &fetched.subject,
                opt_json_param(db, from_json.as_deref()),
                opt_json_param(db, to_json.as_deref()),
                message_date_param(db, fetched.date.as_deref()),
                &fetched.message_id,
                &id_param(db, &row.id)?
            )?;

            row.body_text = fetched.body_text;
            row.body_html = body_html;
            row.has_attachments = fetched.has_attachments || !fetched.attachments.is_empty();
            if row.subject.as_deref().unwrap_or("").is_empty() {
                row.subject = fetched.subject.clone();
            }
            if row.from_address.is_none() {
                row.from_address = from_json;
            }
            if row.to_addresses.is_none() {
                row.to_addresses = to_json;
            }
            if row.date.is_none() {
                row.date = fetched.date.clone();
            }
            if row.snippet.as_deref().unwrap_or("").is_empty() {
                row.snippet = snippet;
            }
            persist_attachments(db, &state.data_dir, &row.id, &fetched.attachments).await?;
        }
    }

    let allow_remote = query.remote_content.as_deref() == Some("allow");
    let response = finalize_message_response(&state, &user_id, &row, allow_remote).await?;
    Ok(Json(response))
}

/// PATCH /api/v1/messages/{id} — update read/starred flags (IMAP STORE when possible).
pub(crate) async fn patch_message(
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
        &id_param(db, &row.id)?
    )?;

    row.is_read = next_read;
    row.is_starred = next_star;

    // Keep folder unread counts roughly consistent.
    update_folder_counts(db, &row.folder_id).await?;

    Ok(Json(
        finalize_message_response(&state, &user_id, &row, false).await?,
    ))
}

/// POST /api/v1/messages/{id}/trash — move to Trash (IMAP) or soft-delete locally.
pub(crate) async fn trash_message(
    State(state): State<AuthState>,
    Path(message_id): Path<String>,
    AuthUser(user_id): AuthUser,
) -> Result<Json<serde_json::Value>, SyncError> {
    move_message_to_role(state, message_id, user_id, "trash").await
}

/// POST /api/v1/messages/{id}/archive — move to Archive when available.
pub(crate) async fn archive_message(
    State(state): State<AuthState>,
    Path(message_id): Path<String>,
    AuthUser(user_id): AuthUser,
) -> Result<Json<serde_json::Value>, SyncError> {
    move_message_to_role(state, message_id, user_id, "archive").await
}

/// POST /api/v1/messages/{id}/snooze — hide locally until `until`, then unsnooze via job.
#[derive(Deserialize)]
pub(crate) struct SnoozeRequest {
    until: String,
}

/// UTC instant as SQLite `datetime()` text (`YYYY-MM-DD HH:MM:SS`).
///
/// Inbox/folder filters compare `snoozed_until <= datetime('now')`. RFC3339 with a `T`
/// separator sorts after the same wall time with a space, so same-day overdue rows would
/// stay hidden if we stored client RFC3339 literally.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn sqlite_utc_datetime(dt: chrono::DateTime<chrono::Utc>) -> String {
    dt.format("%Y-%m-%d %H:%M:%S").to_string()
}

pub(crate) async fn snooze_message(
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
    let until_sql = TsParam::from_utc(db, until_utc);

    db_execute!(
        db,
        r"
        UPDATE message
        SET snoozed_until = CAST(? AS TIMESTAMP), updated_at = datetime('now')
        WHERE id = ?
        ",
        &until_sql,
        &id_param(db, &row.id)?
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

pub(crate) async fn move_message_to_role(
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
            let dest_id = id_from_row(&dest, "id");
            let dest_name: String = dest
                .get::<Option<String>, _>("external_id")
                .unwrap_or_else(|| dest.get("name"));
            (dest_id, dest_name)
        },
        &id_param(db, &row.account_id)?,
        role
    )?;

    let Some((dest_id, dest_name)) = dest else {
        // No destination folder: soft-delete locally.
        db_execute!(
            db,
            "UPDATE message SET is_deleted = ?, updated_at = datetime('now') WHERE id = ?",
            true,
            &id_param(db, &row.id)?
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
        &id_param(db, &dest_id)?,
        &id_param(db, &row.id)?
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
