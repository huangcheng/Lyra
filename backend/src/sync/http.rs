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
use chrono::{DateTime, Utc};
use sea_orm::sea_query::{
    Alias, Condition, Expr, ExprTrait, Func, JoinType, Order, Query as Sq, SelectStatement,
};
use sea_orm::{
    ColumnTrait, ConnectionTrait, EntityTrait, IdenStatic, QueryFilter, QueryResult, QuerySelect,
    Value,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::send::send_message;
use super::store::{effective_folder_role, parse_imap_uid, update_folder_counts};
use super::types::{EnqueuedSync, SyncError, SyncStatus};
use crate::auth::{AuthState, AuthUser};
use crate::db_row::{IdParam, id_param, parse_ts};
use crate::entities::{attachment, folder, jobs, mail_account, message};
use crate::imap::{ImapClient, ImapConfig, ImapSecurity};
use crate::kernel::AppEvent;
use crate::privacy::{
    load_settings, rewrite_remote_images, sender_email_from_json, should_allow_remote,
};
use crate::sanitize::persist_body_html;
use crate::search::{fts_available, search_message_ids};
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
        .route(
            "/api/v1/folders/{folder_id}",
            axum::routing::patch(patch_folder),
        )
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
        .route(
            "/api/v1/attachments/{attachment_id}/download",
            get(download_attachment),
        )
        .route("/api/v1/messages/{message_id}/trash", post(trash_message))
        .route(
            "/api/v1/messages/{message_id}/archive",
            post(archive_message),
        )
        .route("/api/v1/messages/{message_id}/spam", post(spam_message))
        .route("/api/v1/messages/{message_id}/snooze", post(snooze_message))
}

// ── SeaORM seam ─────────────────────────────────────────────────────
//
// Handlers build sea_query statements over entity Columns so the SQL cannot
// drift from the schema. Entity PKs are `Uuid`, but SQLite rows carry legacy
// TEXT ids (tests use `"user-1"` etc.), so ids bind as strings on SQLite and
// native UUIDs on Postgres — the same split `db_row::id_param` makes — and
// read back through dialect-tolerant row decoders below.

/// Unwrap the driver error SeaORM wraps so [`SyncError::Database`] keeps
/// reporting the underlying `sqlx::Error`; non-driver SeaORM errors become
/// `sqlx::Error::Protocol` with the original message.
fn orm_err(err: sea_orm::DbErr) -> SyncError {
    use sea_orm::RuntimeErr;
    let sqlx_err = match err {
        sea_orm::DbErr::Exec(RuntimeErr::SqlxError(e))
        | sea_orm::DbErr::Query(RuntimeErr::SqlxError(e))
        | sea_orm::DbErr::Conn(RuntimeErr::SqlxError(e)) => std::sync::Arc::try_unwrap(e)
            .unwrap_or_else(|shared| sqlx::Error::Protocol(shared.to_string())),
        other => sqlx::Error::Protocol(other.to_string()),
    };
    SyncError::Database(sqlx_err)
}

/// Dialect-aware bind for a UUID-column value: TEXT on SQLite, native UUID on
/// Postgres.
fn id_value(db: &DbPool, id: &str) -> Result<Value, SyncError> {
    Ok(match id_param(db, id)? {
        IdParam::Text(s) => Value::String(Some(s)),
        IdParam::Uuid(u) => Value::Uuid(Some(u)),
    })
}

/// Optional id bind (`InvalidIdError` still maps to 400 on Postgres).
fn opt_id_value(db: &DbPool, id: Option<&str>) -> Result<Option<Value>, SyncError> {
    id.map(|s| id_value(db, s)).transpose()
}

/// Plain text value (`None` becomes a typed NULL).
fn text_value(raw: Option<&str>) -> Value {
    Value::String(raw.map(str::to_owned))
}

/// Owned-string variant of [`text_value`].
fn owned_text_value(raw: Option<String>) -> Value {
    Value::String(raw)
}

/// Typed JSON NULL matching the dialect (TEXT vs JSONB).
fn json_null_value(db: &DbPool) -> Value {
    match db {
        DbPool::Sqlite(_) => Value::String(None),
        #[cfg(feature = "postgres")]
        DbPool::Postgres(_) => Value::Json(None),
    }
}

/// Bind optional JSON text for JSONB/TEXT columns (lenient like the macro
/// layer's [`crate::db_row::JsonParam`]: non-JSON strings stay raw on SQLite
/// and become string scalars on Postgres).
fn opt_json_value(db: &DbPool, raw: Option<&str>) -> Value {
    let Some(raw) = raw else {
        return json_null_value(db);
    };
    match db {
        DbPool::Sqlite(_) => Value::String(Some(raw.to_owned())),
        #[cfg(feature = "postgres")]
        DbPool::Postgres(_) => Value::Json(Some(Box::new(
            serde_json::from_str(raw).unwrap_or_else(|_| serde_json::Value::String(raw.to_owned())),
        ))),
    }
}

/// Bind an optional UTC instant for `TIMESTAMPTZ` / TEXT timestamp columns,
/// shaped like the legacy `datetime()` writers.
fn ts_value(db: &DbPool, dt: Option<DateTime<Utc>>) -> Value {
    match db {
        DbPool::Sqlite(_) => Value::String(dt.map(|d| d.format("%Y-%m-%d %H:%M:%S").to_string())),
        #[cfg(feature = "postgres")]
        DbPool::Postgres(_) => Value::ChronoDateTimeUtc(dt),
    }
}

/// `updated_at` write, shaped like the legacy `datetime('now')` / `NOW()`
/// defaults so sqlite rows keep their `YYYY-MM-DD HH:MM:SS` text format.
fn now_value(db: &DbPool) -> Value {
    match db {
        DbPool::Sqlite(_) => {
            Value::String(Some(Utc::now().format("%Y-%m-%d %H:%M:%S").to_string()))
        }
        #[cfg(feature = "postgres")]
        DbPool::Postgres(_) => Value::ChronoDateTimeUtc(Some(Utc::now())),
    }
}

fn missing_column(col: &str) -> sea_orm::DbErr {
    sea_orm::DbErr::Query(sea_orm::RuntimeErr::Internal(format!(
        "missing column {col}"
    )))
}

/// Decode a UUID/TEXT id column: `String` on SQLite, native UUID on Postgres.
fn row_id(row: &QueryResult, col: &str) -> Result<String, sea_orm::DbErr> {
    if let Some(s) = row.try_get::<Option<String>>("", col).ok().flatten() {
        return Ok(s);
    }
    row.try_get::<Option<Uuid>>("", col)?
        .map(|u| u.to_string())
        .ok_or_else(|| missing_column(col))
}

/// Nullable id column ([`row_id`] semantics).
fn row_opt_id(row: &QueryResult, col: &str) -> Result<Option<String>, sea_orm::DbErr> {
    if let Ok(text) = row.try_get::<Option<String>>("", col) {
        return Ok(text);
    }
    Ok(row.try_get::<Option<Uuid>>("", col)?.map(|u| u.to_string()))
}

/// Nullable timestamp column: stored text on SQLite, RFC3339 on Postgres.
fn row_opt_ts(row: &QueryResult, col: &str) -> Result<Option<String>, sea_orm::DbErr> {
    if let Ok(text) = row.try_get::<Option<String>>("", col) {
        return Ok(text);
    }
    row.try_get::<Option<DateTime<Utc>>>("", col)
        .map(|opt| opt.map(|t| t.to_rfc3339()))
}

/// JSONB / TEXT json → JSON text for the API (`from_address`, …).
///
/// Stored TEXT is returned verbatim when present (SQLite keeps raw header
/// text too); Postgres falls back to native JSONB decode.
fn row_json_text(row: &QueryResult, col: &str) -> Result<Option<String>, sea_orm::DbErr> {
    if let Ok(text) = row.try_get::<Option<String>>("", col) {
        return Ok(text);
    }
    let value: Option<serde_json::Value> = row.try_get("", col)?;
    Ok(value.filter(|v| !v.is_null()).map(|v| v.to_string()))
}

/// Qualified projection expression for the aliased message table (`m.<col>`).
fn m_col(col: message::Column) -> Expr {
    Expr::col((Alias::new("m"), Alias::new(col.as_str())))
}

/// Message columns shared by every listing handler, projected as
/// `m.<col> AS <col>` with entity-owned names.
const MESSAGE_LIST_COLS: &[message::Column] = &[
    message::Column::Id,
    message::Column::AccountId,
    message::Column::FolderId,
    message::Column::MessageIdHeader,
    message::Column::Subject,
    message::Column::FromAddress,
    message::Column::ToAddresses,
    message::Column::CcAddresses,
    message::Column::Date,
    message::Column::Snippet,
    message::Column::BodyText,
    message::Column::BodyHtml,
    message::Column::IsRead,
    message::Column::IsStarred,
    message::Column::HasAttachments,
];

fn add_message_list_columns(query: &mut SelectStatement) {
    for col in MESSAGE_LIST_COLS {
        query.expr_as(m_col(*col), Alias::new(col.as_str()));
    }
}

/// `FROM message AS m JOIN mail_account AS a ON m.account_id = a.id`.
fn add_message_account_join(query: &mut SelectStatement) {
    query.from_as(message::Entity, Alias::new("m")).join_as(
        JoinType::InnerJoin,
        mail_account::Entity,
        Alias::new("a"),
        Expr::cust("m.account_id = a.id"),
    );
}

/// Additionally `JOIN folder AS f ON m.folder_id = f.id`.
fn add_message_folder_join(query: &mut SelectStatement) {
    query.join_as(
        JoinType::InnerJoin,
        folder::Entity,
        Alias::new("f"),
        Expr::cust("m.folder_id = f.id"),
    );
}

/// Soft-deleted filter, bound through the entity-shaped column name.
const NOT_DELETED_SQL: &str = "m.is_deleted = ?";

fn not_deleted_clause() -> Expr {
    Expr::cust_with_values(NOT_DELETED_SQL, [false])
}

/// Snooze visibility kept explicitly dialect-branched: SQLite compares
/// `snoozed_until` text against `datetime('now')`; Postgres compares
/// TIMESTAMPTZ against NOW().
const SNOOZE_VISIBLE_SQLITE: &str =
    "(m.snoozed_until IS NULL OR m.snoozed_until <= datetime('now'))";
#[cfg(feature = "postgres")]
const SNOOZE_VISIBLE_POSTGRES: &str = "(m.snoozed_until IS NULL OR m.snoozed_until <= NOW())";

fn snooze_visible_clause(db: &DbPool) -> Expr {
    match db {
        DbPool::Sqlite(_) => Expr::cust(SNOOZE_VISIBLE_SQLITE),
        #[cfg(feature = "postgres")]
        DbPool::Postgres(_) => Expr::cust(SNOOZE_VISIBLE_POSTGRES),
    }
}

fn message_response_from_query_row(row: &QueryResult) -> Result<MessageResponse, sea_orm::DbErr> {
    Ok(MessageResponse {
        id: row_id(row, "id")?,
        account_id: row_id(row, "account_id")?,
        folder_id: row_id(row, "folder_id")?,
        message_id_header: row.try_get("", "message_id_header")?,
        subject: row
            .try_get::<Option<String>>("", "subject")?
            .map(|s| crate::imap::decode_mime_header(&s)),
        from_address: row_json_text(row, "from_address")?
            .map(|s| crate::imap::decode_mime_header(&s)),
        to_addresses: row_json_text(row, "to_addresses")?
            .map(|s| crate::imap::decode_mime_header(&s)),
        cc_addresses: row_json_text(row, "cc_addresses")?
            .map(|s| crate::imap::decode_mime_header(&s)),
        date: row_opt_ts(row, "date")?,
        snippet: row
            .try_get::<Option<String>>("", "snippet")?
            .map(|s| crate::imap::decode_mime_header(&s)),
        body_text: row.try_get("", "body_text")?,
        body_html: row.try_get("", "body_html")?,
        is_read: row.try_get("", "is_read")?,
        is_starred: row.try_get("", "is_starred")?,
        has_attachments: row.try_get("", "has_attachments")?,
        remote_content_blocked: false,
        opengpg: None,
    })
}

/// Build + run one of the listing queries and map rows to the API DTO.
async fn run_message_list(
    db: &DbPool,
    build: impl FnOnce(&mut SelectStatement),
) -> Result<Vec<MessageResponse>, SyncError> {
    let mut query = Sq::select();
    add_message_list_columns(&mut query);
    build(&mut query);

    let rows = db.orm().query_all(&query).await.map_err(orm_err)?;
    rows.iter()
        .map(message_response_from_query_row)
        .collect::<Result<Vec<_>, _>>()
        .map_err(orm_err)
}

/// Fetch the first row of a probe/aggregating statement, or `None`.
async fn query_first(
    db: &DbPool,
    build: impl FnOnce(&mut SelectStatement),
) -> Result<Option<QueryResult>, SyncError> {
    let mut query = Sq::select();
    build(&mut query);
    db.orm().query_one(&query).await.map_err(orm_err)
}

// ── Handlers ────────────────────────────────────────────────────────

/// Get sync status.
pub(crate) async fn sync_status(
    State(state): State<AuthState>,
    AuthUser(user_id): AuthUser,
) -> Result<Json<SyncStatus>, SyncError> {
    let db = state.db();
    let user_value = id_value(db, &user_id)?;

    let row = query_first(db, |q| {
        q.expr_as(Expr::cust("COUNT(*)"), Alias::new("c"))
            .from(mail_account::Entity)
            .and_where(Expr::col(mail_account::Column::UserId).eq(user_value.clone()))
            .and_where(Expr::col(mail_account::Column::IsActive).eq(true))
            .and_where(Expr::col(mail_account::Column::SyncEnabled).eq(true));
    })
    .await?
    .ok_or_else(|| SyncError::Internal("count query returned no rows".into()))?;
    let count: i64 = row.try_get("", "c").map_err(orm_err)?;

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
    // `jobs.id` is TEXT in both dialects, so this entity supports direct use.
    let payloads: Vec<String> = jobs::Entity::find()
        .filter(jobs::Column::Kind.eq("sync_account"))
        .filter(jobs::Column::Status.is_in(["pending", "running"]))
        .select_only()
        .column(jobs::Column::Payload)
        .into_tuple::<String>()
        .all(&db.orm())
        .await
        .map_err(orm_err)?;

    for payload_json in payloads {
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
    let account_value = id_value(db, &account_id)?;
    let user_value = id_value(db, &user_id)?;

    let exists = query_first(db, |q| {
        q.expr_as(Expr::col(mail_account::Column::Id), Alias::new("id"))
            .from(mail_account::Entity)
            .and_where(Expr::col(mail_account::Column::Id).eq(account_value.clone()))
            .and_where(Expr::col(mail_account::Column::UserId).eq(user_value.clone()));
    })
    .await?;
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
    /// Effective role: `COALESCE(role_override, role)`.
    pub role: Option<String>,
    /// Explicit local override (null = use detected SPECIAL-USE / name).
    pub role_override: Option<String>,
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
    /// RFC 5322 Message-ID — clients use it to recognize cross-folder
    /// copies of the same message (e.g. INBOX + Archive).
    pub message_id_header: Option<String>,
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
    /// OpenGPG decrypt/verify status when the message looks encrypted or signed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub opengpg: Option<crate::opengpg::OpengpgMessageStatus>,
}

/// Folder columns needed by [`FolderResponse`], projected from alias `f`.
const FOLDER_COLS: &[folder::Column] = &[
    folder::Column::Id,
    folder::Column::AccountId,
    folder::Column::Name,
    folder::Column::Role,
    folder::Column::RoleOverride,
    folder::Column::ParentId,
    folder::Column::SortOrder,
    folder::Column::TotalMessages,
    folder::Column::UnreadMessages,
];

fn add_folder_columns(query: &mut SelectStatement) {
    for col in FOLDER_COLS {
        query.expr_as(
            Expr::col((Alias::new("f"), Alias::new(col.as_str()))),
            Alias::new(col.as_str()),
        );
    }
}

fn folder_response_from_row(row: &QueryResult) -> Result<FolderResponse, sea_orm::DbErr> {
    let detected: Option<String> = row.try_get("", "role")?;
    let override_role: Option<String> = row.try_get("", "role_override")?;
    Ok(FolderResponse {
        id: row_id(row, "id")?,
        account_id: row_id(row, "account_id")?,
        name: row.try_get("", "name")?,
        role: effective_folder_role(detected.as_deref(), override_role.as_deref()),
        role_override: override_role,
        parent_id: row_opt_id(row, "parent_id")?,
        sort_order: row.try_get("", "sort_order")?,
        total_messages: row.try_get("", "total_messages")?,
        unread_messages: row.try_get("", "unread_messages")?,
    })
}

/// List all folders for the authenticated user.
pub(crate) async fn list_folders(
    State(state): State<AuthState>,
    AuthUser(user_id): AuthUser,
) -> Result<Json<Vec<FolderResponse>>, SyncError> {
    let db = state.db();
    let user_value = id_value(db, &user_id)?;

    let mut query = Sq::select();
    add_folder_columns(&mut query);
    query
        .from_as(folder::Entity, Alias::new("f"))
        .join_as(
            JoinType::InnerJoin,
            mail_account::Entity,
            Alias::new("a"),
            Expr::cust("f.account_id = a.id"),
        )
        .and_where(Expr::cust_with_values("a.user_id = ?", [user_value]))
        .order_by_expr(
            Expr::col((Alias::new("f"), Alias::new("sort_order"))),
            Order::Asc,
        )
        .order_by_expr(Expr::col((Alias::new("f"), Alias::new("name"))), Order::Asc);

    let rows = db.orm().query_all(&query).await.map_err(orm_err)?;
    let folders = rows
        .iter()
        .map(folder_response_from_row)
        .collect::<Result<Vec<_>, _>>()
        .map_err(orm_err)?;

    Ok(Json(folders))
}

/// Allowed values for `role_override` / SPECIAL-USE remapping.
const OVERRIDE_ROLES: &[&str] = &["inbox", "sent", "drafts", "trash", "spam", "archive"];

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PatchFolderRequest {
    /// Set override role; omit with `clearRoleOverride: true` to clear.
    pub role_override: Option<String>,
    #[serde(default)]
    pub clear_role_override: bool,
}

/// PATCH /api/v1/folders/{folder_id} — set or clear a local role override.
#[allow(clippy::too_many_lines)] // three-way override mutation + re-fetch, mirroring the SQL shape
pub(crate) async fn patch_folder(
    State(state): State<AuthState>,
    Path(folder_id): Path<String>,
    AuthUser(user_id): AuthUser,
    Json(body): Json<PatchFolderRequest>,
) -> Result<Json<FolderResponse>, SyncError> {
    let db = state.db();
    let folder_value = id_value(db, &folder_id)?;
    let user_value = id_value(db, &user_id)?;

    let account_row = query_first(db, |q| {
        q.expr_as(
            Expr::col((
                Alias::new("f"),
                Alias::new(folder::Column::AccountId.as_str()),
            )),
            Alias::new("account_id"),
        )
        .from_as(folder::Entity, Alias::new("f"))
        .join_as(
            JoinType::InnerJoin,
            mail_account::Entity,
            Alias::new("a"),
            Expr::cust("f.account_id = a.id"),
        )
        .and_where(Expr::cust_with_values("f.id = ?", [folder_value.clone()]))
        .and_where(Expr::cust_with_values("a.user_id = ?", [user_value]));
    })
    .await?
    .ok_or(SyncError::InvalidInput("folder not found".into()))?;
    let account_id = row_id(&account_row, "account_id").map_err(orm_err)?;

    if !body.clear_role_override && body.role_override.is_none() {
        return Err(SyncError::InvalidInput(
            "provide roleOverride or clearRoleOverride".into(),
        ));
    }

    let pushed_override: Option<Option<String>> = if body.clear_role_override {
        let mut update = Sq::update();
        update
            .table(folder::Entity)
            .value(folder::Column::RoleOverride, Value::String(None))
            .value(folder::Column::UpdatedAt, now_value(db))
            .and_where(Expr::col(folder::Column::Id).eq(folder_value.clone()));
        db.orm().execute(&update).await.map_err(orm_err)?;
        Some(None)
    } else if let Some(ref role) = body.role_override {
        if !OVERRIDE_ROLES.contains(&role.as_str()) {
            return Err(SyncError::InvalidInput(format!(
                "roleOverride must be one of: {}",
                OVERRIDE_ROLES.join(", ")
            )));
        }
        // One folder per override role per account.
        let mut demote = Sq::update();
        demote
            .table(folder::Entity)
            .value(folder::Column::RoleOverride, Value::String(None))
            .value(folder::Column::UpdatedAt, now_value(db))
            .and_where(Expr::col(folder::Column::AccountId).eq(id_value(db, &account_id)?))
            .and_where(Expr::col(folder::Column::RoleOverride).eq(role.as_str()))
            .and_where(Expr::col(folder::Column::Id).ne(folder_value.clone()));
        db.orm().execute(&demote).await.map_err(orm_err)?;
        let mut promote = Sq::update();
        promote
            .table(folder::Entity)
            .value(folder::Column::RoleOverride, text_value(Some(role)))
            .value(folder::Column::UpdatedAt, now_value(db))
            .and_where(Expr::col(folder::Column::Id).eq(folder_value.clone()));
        db.orm().execute(&promote).await.map_err(orm_err)?;
        Some(Some(role.clone()))
    } else {
        None
    };

    if let Some(override_role) = pushed_override {
        best_effort_push_specialuse(
            db,
            &user_id,
            &account_id,
            &folder_id,
            override_role.as_deref(),
        )
        .await;
    }

    let mut select = Sq::select();
    add_folder_columns(&mut select);
    select
        .from_as(folder::Entity, Alias::new("f"))
        .and_where(Expr::cust_with_values("f.id = ?", [folder_value]));

    let row = db
        .orm()
        .query_one(&select)
        .await
        .map_err(orm_err)?
        .ok_or_else(|| SyncError::InvalidInput("folder not found".into()))?;
    let folder = folder_response_from_row(&row).map_err(orm_err)?;

    Ok(Json(folder))
}

/// Best-effort IMAP METADATA push for a local role override (CHE-128 layer 2).
///
/// Never fails the HTTP response: unsupported servers, connect errors, and
/// `NO [USEATTR]` are all silent.
async fn best_effort_push_specialuse(
    db: &DbPool,
    user_id: &str,
    account_id: &str,
    folder_id: &str,
    role_override: Option<&str>,
) {
    let Ok(folder_value) = id_value(db, folder_id) else {
        return;
    };
    let external_id = query_first(db, |q| {
        q.expr_as(
            Expr::col(folder::Column::ExternalId),
            Alias::new("external_id"),
        )
        .from(folder::Entity)
        .and_where(Expr::col(folder::Column::Id).eq(folder_value));
    })
    .await
    .ok()
    .flatten()
    .and_then(|row| {
        row.try_get::<Option<String>>("", "external_id")
            .ok()
            .flatten()
    });
    let Some(external_id) = external_id else {
        return;
    };

    let Ok((mut client, _)) = connect_imap_for_account(db, user_id, account_id).await else {
        return;
    };

    let special = role_override.and_then(crate::imap::role_to_specialuse);
    let _ = client.set_private_specialuse(&external_id, special).await;
    let _ = client.logout().await;
}

/// List messages in a folder.
pub(crate) async fn list_messages(
    State(state): State<AuthState>,
    Path(folder_id): Path<String>,
    AuthUser(user_id): AuthUser,
) -> Result<Json<Vec<MessageResponse>>, SyncError> {
    let db = state.db();
    let folder_value = id_value(db, &folder_id)?;
    let user_value = id_value(db, &user_id)?;

    // Verify folder belongs to the user
    let check = query_first(db, |q| {
        q.expr_as(
            Expr::col((Alias::new("f"), Alias::new(folder::Column::Id.as_str()))),
            Alias::new("id"),
        )
        .from_as(folder::Entity, Alias::new("f"))
        .join_as(
            JoinType::InnerJoin,
            mail_account::Entity,
            Alias::new("a"),
            Expr::cust("f.account_id = a.id"),
        )
        .and_where(Expr::cust_with_values("f.id = ?", [folder_value.clone()]))
        .and_where(Expr::cust_with_values("a.user_id = ?", [user_value]));
    })
    .await?;
    if check.is_none() {
        return Err(SyncError::AccountNotFound);
    }

    let messages = run_message_list(db, |q| {
        q.from_as(message::Entity, Alias::new("m"))
            .and_where(Expr::cust_with_values("m.folder_id = ?", [folder_value]))
            .and_where(not_deleted_clause())
            .and_where(snooze_visible_clause(db))
            .order_by_expr(Expr::cust("m.date"), Order::Desc)
            .limit(500);
    })
    .await?;

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
///
/// Optional filters become conditional `WHERE`s instead of the legacy
/// `(? IS NULL OR … = ?)` duality — the resulting predicate set is identical.
pub(crate) async fn query_user_messages(
    db: &DbPool,
    user_id: &str,
    role: Option<&str>,
    account_id: Option<&str>,
) -> Result<Vec<MessageResponse>, SyncError> {
    let user_value = id_value(db, user_id)?;
    let account_value = opt_id_value(db, account_id)?;

    run_message_list(db, |q| {
        add_message_account_join(q);
        add_message_folder_join(q);
        q.and_where(Expr::cust_with_values("a.user_id = ?", [user_value]))
            .and_where(not_deleted_clause())
            .and_where(snooze_visible_clause(db));
        if let Some(role) = role {
            q.and_where(Expr::cust_with_values(
                "COALESCE(f.role_override, f.role) = ?",
                [role],
            ));
        }
        if let Some(account_value) = account_value {
            q.and_where(Expr::cust_with_values("m.account_id = ?", [account_value]));
        }
        q.order_by_expr(Expr::cust("m.date"), Order::Desc)
            .limit(500);
    })
    .await
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

/// Search messages by subject / body / from (local FTS index, LIKE fallback).
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

    let limit = query.limit.unwrap_or(100).clamp(1, 500);
    let user_value = id_value(db, &user_id)?;
    let account_value = opt_id_value(db, query.account_id.as_deref())?;

    let messages = if fts_available(db).await? {
        let ids = search_message_ids(
            db,
            &user_id,
            q,
            query.account_id.as_deref(),
            query.folder_id.as_deref(),
            limit,
        )
        .await
        .map_err(|e| match e {
            crate::search::SearchError::InvalidQuery | crate::search::SearchError::InvalidId(_) => {
                SyncError::InvalidInput("invalid search query".into())
            }
            crate::search::SearchError::Database(err) => SyncError::Database(err),
        })?;
        if ids.is_empty() {
            Vec::new()
        } else {
            fetch_messages_by_ids(db, &ids, &user_id).await?
        }
    } else {
        search_like_fallback(
            db,
            q,
            user_value,
            account_value,
            query.folder_id.as_deref(),
            limit,
        )
        .await?
    };

    Ok(Json(messages))
}

/// LIKE fallback branch (used only when the FTS index is unavailable).
///
/// The keyword spelling follows the legacy rewrite exactly (SQLite `LIKE`,
/// already case-insensitive for ASCII; Postgres `ILIKE`), so the templates are
/// static strings chosen by dialect; the pattern binds through the trailing
/// placeholder.
async fn search_like_fallback(
    db: &DbPool,
    q: &str,
    user_value: Value,
    account_value: Option<Value>,
    folder_id: Option<&str>,
    limit: i64,
) -> Result<Vec<MessageResponse>, SyncError> {
    const LIKE_SQLITE: [&str; 4] = [
        "COALESCE(m.subject, '') LIKE ?",
        "COALESCE(m.snippet, '') LIKE ?",
        "COALESCE(m.body_text, '') LIKE ?",
        "COALESCE(m.from_address, '') LIKE ?",
    ];
    #[cfg(feature = "postgres")]
    const LIKE_POSTGRES: [&str; 4] = [
        "COALESCE(m.subject, '') ILIKE ?",
        "COALESCE(m.snippet, '') ILIKE ?",
        "COALESCE(m.body_text, '') ILIKE ?",
        "COALESCE(m.from_address, '') ILIKE ?",
    ];

    let templates: &[&str] = match db {
        DbPool::Sqlite(_) => &LIKE_SQLITE,
        #[cfg(feature = "postgres")]
        DbPool::Postgres(_) => &LIKE_POSTGRES,
    };

    let folder_value = opt_id_value(db, folder_id)?;
    let pattern = format!("%{q}%");

    let mut any = Condition::any();
    for template in templates {
        any = any.add(Expr::cust_with_values(*template, [pattern.clone()]));
    }

    run_message_list(db, |sel| {
        add_message_account_join(sel);
        sel.and_where(Expr::cust_with_values("a.user_id = ?", [user_value]))
            .and_where(not_deleted_clause());
        if let Some(account_value) = account_value {
            sel.and_where(Expr::cust_with_values("m.account_id = ?", [account_value]));
        }
        if let Some(folder_value) = folder_value {
            sel.and_where(Expr::cust_with_values("m.folder_id = ?", [folder_value]));
        }
        sel.cond_where(any)
            .order_by_expr(Expr::cust("m.date"), Order::Desc)
            .limit(u64::try_from(limit).unwrap_or(500));
    })
    .await
}

/// Fetch FTS hits (in rank order), mapping each to a response the requesting
/// user owns and that is not soft-deleted.
async fn fetch_messages_by_ids(
    db: &DbPool,
    ids: &[String],
    user_id: &str,
) -> Result<Vec<MessageResponse>, SyncError> {
    let user_value = id_value(db, user_id)?;
    let mut messages = Vec::with_capacity(ids.len());
    for id in ids {
        let msg_value = id_value(db, id)?;
        let row = query_first(db, |q| {
            add_message_list_columns(q);
            add_message_account_join(q);
            q.and_where(Expr::cust_with_values("m.id = ?", [msg_value.clone()]))
                .and_where(Expr::cust_with_values(
                    "a.user_id = ?",
                    [user_value.clone()],
                ))
                .and_where(not_deleted_clause());
        })
        .await?;
        if let Some(row) = row {
            messages.push(message_response_from_query_row(&row).map_err(orm_err)?);
        }
    }
    Ok(messages)
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
    let message_value = id_value(db, &message_id)?;

    let rows = db
        .orm()
        .query_all(&{
            let mut q = Sq::select();
            q.columns([
                attachment::Column::Id,
                attachment::Column::MessageId,
                attachment::Column::Filename,
                attachment::Column::ContentType,
                attachment::Column::SizeBytes,
                attachment::Column::IsInline,
            ])
            .from(attachment::Entity)
            .and_where(Expr::col(attachment::Column::MessageId).eq(message_value))
            .order_by_expr(Expr::col(attachment::Column::CreatedAt), Order::Asc);
            q
        })
        .await
        .map_err(orm_err)?;

    let attachments = rows
        .iter()
        .map(|row| {
            Ok(AttachmentResponse {
                id: row_id(row, "id")?,
                message_id: row_id(row, "message_id")?,
                filename: row.try_get("", "filename")?,
                content_type: row.try_get("", "content_type")?,
                size_bytes: row.try_get("", "size_bytes")?,
                is_inline: row.try_get("", "is_inline")?,
            })
        })
        .collect::<Result<Vec<_>, sea_orm::DbErr>>()
        .map_err(orm_err)?;

    Ok(Json(attachments))
}

pub(crate) async fn download_attachment(
    State(state): State<AuthState>,
    Path(attachment_id): Path<String>,
    AuthUser(user_id): AuthUser,
) -> Result<Response, SyncError> {
    let db = state.db();
    let att_value = id_value(db, &attachment_id)?;
    let user_value = id_value(db, &user_id)?;

    let row = query_first(db, |q| {
        q.expr_as(
            Expr::col((Alias::new("a"), Alias::new("storage_path"))),
            Alias::new("storage_path"),
        )
        .expr_as(
            Expr::col((Alias::new("a"), Alias::new("filename"))),
            Alias::new("filename"),
        )
        .expr_as(
            Expr::col((Alias::new("a"), Alias::new("content_type"))),
            Alias::new("content_type"),
        )
        .from_as(attachment::Entity, Alias::new("a"))
        .join_as(
            JoinType::InnerJoin,
            message::Entity,
            Alias::new("m"),
            Expr::cust("a.message_id = m.id"),
        )
        .join_as(
            JoinType::InnerJoin,
            mail_account::Entity,
            Alias::new("acc"),
            Expr::cust("m.account_id = acc.id"),
        )
        .and_where(Expr::cust_with_values("a.id = ?", [att_value]))
        .and_where(Expr::cust_with_values("acc.user_id = ?", [user_value]));
    })
    .await?
    .ok_or(SyncError::MessageNotFound)?;

    let storage_path: String = row.try_get("", "storage_path").map_err(orm_err)?;
    let filename: Option<String> = row.try_get("", "filename").map_err(orm_err)?;
    let content_type: Option<String> = row.try_get("", "content_type").map_err(orm_err)?;

    let bytes = crate::blobs::read(&state.data_dir, &storage_path)
        .await
        .map_err(|e| {
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
    account_id: &str,
    message_id: &str,
    attachments: &[crate::imap::ExtractedAttachment],
) -> Result<(), SyncError> {
    if attachments.is_empty() {
        return Ok(());
    }
    let message_value = id_value(db, message_id)?;
    let conn = db.orm();

    // Replace prior attachment rows for this message (re-fetch).
    //
    // Deliberately kept non-transactional exactly as before: wrapping the
    // DELETE + blob-writing INSERTs in a transaction would hold the write lock
    // across blob I/O and change mid-backfill failure visibility.
    let mut delete = Sq::delete();
    delete
        .from_table(attachment::Entity)
        .and_where(Expr::col(attachment::Column::MessageId).eq(message_value.clone()));
    conn.execute(&delete).await.map_err(orm_err)?;

    for att in attachments {
        let id = Uuid::new_v7(uuid::Timestamp::now(uuid::NoContext)).to_string();
        let storage_path = crate::blobs::store(data_dir, account_id, &att.data)
            .await
            .map_err(|e| SyncError::InvalidInput(format!("cannot write attachment blob: {e}")))?;

        let size = i64::try_from(att.data.len()).unwrap_or(i64::MAX);
        let mut insert = Sq::insert();
        insert
            .into_table(attachment::Entity)
            .columns([
                attachment::Column::Id,
                attachment::Column::MessageId,
                attachment::Column::Filename,
                attachment::Column::ContentType,
                attachment::Column::SizeBytes,
                attachment::Column::StoragePath,
                attachment::Column::ContentId,
                attachment::Column::IsInline,
            ])
            .values_panic([
                id_value(db, &id)?.into(),
                message_value.clone().into(),
                Value::String(Some(att.filename.clone())).into(),
                Value::String(Some(att.content_type.clone())).into(),
                Value::BigInt(Some(size)).into(),
                Value::String(Some(storage_path)).into(),
                text_value(att.content_id.as_deref()).into(),
                Value::Bool(Some(att.is_inline)).into(),
            ]);
        conn.execute(&insert).await.map_err(orm_err)?;
    }

    let mut touch = Sq::update();
    touch
        .table(message::Entity)
        .value(message::Column::HasAttachments, true)
        .value(message::Column::UpdatedAt, now_value(db))
        .and_where(Expr::col(message::Column::Id).eq(message_value));
    conn.execute(&touch).await.map_err(orm_err)?;

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
    message_id_header: Option<String>,
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
    size_bytes: Option<i64>,
}

pub(crate) fn message_response_from_row(row: &MessageRow) -> MessageResponse {
    MessageResponse {
        id: row.id.clone(),
        account_id: row.account_id.clone(),
        folder_id: row.folder_id.clone(),
        message_id_header: row.message_id_header.clone(),
        subject: row.subject.as_deref().map(crate::imap::decode_mime_header),
        from_address: row
            .from_address
            .as_deref()
            .map(crate::imap::decode_mime_header),
        to_addresses: row
            .to_addresses
            .as_deref()
            .map(crate::imap::decode_mime_header),
        cc_addresses: row
            .cc_addresses
            .as_deref()
            .map(crate::imap::decode_mime_header),
        date: row.date.clone(),
        snippet: row.snippet.as_deref().map(crate::imap::decode_mime_header),
        body_text: row.body_text.clone(),
        body_html: row.body_html.clone(),
        is_read: row.is_read,
        is_starred: row.is_starred,
        has_attachments: row.has_attachments,
        remote_content_blocked: false,
        opengpg: None,
    }
}

fn header_needs_refresh(existing: Option<&str>) -> bool {
    let value = existing.unwrap_or("");
    value.is_empty() || (value.contains("=?") && value.contains("?="))
}

/// Apply remote-image policy and optional OpenGPG decrypt at serve time.
pub(crate) async fn finalize_message_response_with_opengpg(
    state: &AuthState,
    user_id: &str,
    session_token: Option<&str>,
    row: &MessageRow,
    remote_content_allow: bool,
) -> Result<MessageResponse, SyncError> {
    let mut response = message_response_from_row(row);

    if let Some(token) = session_token {
        match crate::opengpg::enrich_message_opengpg(
            state,
            user_id,
            token,
            &row.id,
            response.body_text.as_deref(),
            response.body_html.as_deref(),
        )
        .await
        {
            Ok(Some(decrypted)) => {
                if decrypted.status.decrypted || !decrypted.status.encrypted {
                    if decrypted.body_text.is_some() {
                        response.body_text = decrypted.body_text;
                    }
                    if decrypted.body_html.is_some() {
                        response.body_html = decrypted.body_html;
                    }
                }
                response.opengpg = Some(decrypted.status);
            }
            Ok(None) => {}
            Err(e) => {
                tracing::warn!(error = %e, "opengpg enrich failed");
                response.opengpg = Some(crate::opengpg::OpengpgMessageStatus {
                    encrypted: true,
                    decrypted: false,
                    signatures: Vec::new(),
                    error: Some("opengpg error".into()),
                });
            }
        }
    }

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

/// Message columns loaded by [`load_message_row`] (plus `f.external_id AS
/// folder_name` and `a.protocol` joined below).
const MESSAGE_LOAD_COLS: &[message::Column] = &[
    message::Column::Id,
    message::Column::AccountId,
    message::Column::FolderId,
    message::Column::ExternalId,
    message::Column::MessageIdHeader,
    message::Column::Subject,
    message::Column::FromAddress,
    message::Column::ToAddresses,
    message::Column::CcAddresses,
    message::Column::Date,
    message::Column::Snippet,
    message::Column::BodyText,
    message::Column::BodyHtml,
    message::Column::IsRead,
    message::Column::IsStarred,
    message::Column::HasAttachments,
    message::Column::SizeBytes,
];

pub(crate) async fn load_message_row(
    db: &DbPool,
    user_id: &str,
    message_id: &str,
) -> Result<MessageRow, SyncError> {
    let msg_value = id_value(db, message_id)?;
    let user_value = id_value(db, user_id)?;

    let mut query = Sq::select();
    for col in MESSAGE_LOAD_COLS {
        query.expr_as(m_col(*col), Alias::new(col.as_str()));
    }
    query
        .expr_as(Expr::cust("f.external_id"), Alias::new("folder_name"))
        .expr_as(Expr::cust("a.protocol"), Alias::new("protocol"));
    add_message_account_join(&mut query);
    add_message_folder_join(&mut query);
    query
        .and_where(Expr::cust_with_values("m.id = ?", [msg_value]))
        .and_where(Expr::cust_with_values("a.user_id = ?", [user_value]))
        .and_where(not_deleted_clause());

    let row = db
        .orm()
        .query_one(&query)
        .await
        .map_err(orm_err)?
        .ok_or(SyncError::MessageNotFound)?;

    Ok(MessageRow {
        id: row_id(&row, "id").map_err(orm_err)?,
        account_id: row_id(&row, "account_id").map_err(orm_err)?,
        folder_id: row_id(&row, "folder_id").map_err(orm_err)?,
        folder_name: row.try_get("", "folder_name").map_err(orm_err)?,
        external_id: row.try_get("", "external_id").map_err(orm_err)?,
        message_id_header: row.try_get("", "message_id_header").map_err(orm_err)?,
        protocol: row.try_get("", "protocol").map_err(orm_err)?,
        body_text: row.try_get("", "body_text").map_err(orm_err)?,
        body_html: row.try_get("", "body_html").map_err(orm_err)?,
        is_read: row.try_get("", "is_read").map_err(orm_err)?,
        is_starred: row.try_get("", "is_starred").map_err(orm_err)?,
        subject: row.try_get("", "subject").map_err(orm_err)?,
        from_address: row_json_text(&row, "from_address").map_err(orm_err)?,
        to_addresses: row_json_text(&row, "to_addresses").map_err(orm_err)?,
        cc_addresses: row_json_text(&row, "cc_addresses").map_err(orm_err)?,
        date: row_opt_ts(&row, "date").map_err(orm_err)?,
        snippet: row.try_get("", "snippet").map_err(orm_err)?,
        has_attachments: row.try_get("", "has_attachments").map_err(orm_err)?,
        size_bytes: row.try_get("", "size_bytes").map_err(orm_err)?,
    })
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
    let acct_value = id_value(db, account_id)?;
    let user_value = id_value(db, user_id)?;

    let row = query_first(db, |q| {
        q.expr_as(
            Expr::col(mail_account::Column::Protocol),
            Alias::new("protocol"),
        )
        .expr_as(
            Expr::col(mail_account::Column::ImapHost),
            Alias::new("imap_host"),
        )
        .expr_as(
            Expr::col(mail_account::Column::ImapPort),
            Alias::new("imap_port"),
        )
        .expr_as(
            Expr::col(mail_account::Column::ImapSecurity),
            Alias::new("imap_security"),
        )
        .expr_as(
            Expr::col(mail_account::Column::EmailAddress),
            Alias::new("email_address"),
        )
        .expr_as(
            Expr::col(mail_account::Column::AuthType),
            Alias::new("auth_type"),
        )
        .from(mail_account::Entity)
        .and_where(Expr::col(mail_account::Column::Id).eq(acct_value))
        .and_where(Expr::col(mail_account::Column::UserId).eq(user_value))
        .and_where(Expr::col(mail_account::Column::IsActive).eq(true));
    })
    .await?
    .ok_or(SyncError::AccountNotFound)?;

    let protocol: String = row.try_get("", "protocol").map_err(orm_err)?;
    let imap_host: Option<String> = row.try_get("", "imap_host").map_err(orm_err)?;
    let imap_port: Option<i32> = row.try_get("", "imap_port").map_err(orm_err)?;
    let imap_security: Option<String> = row.try_get("", "imap_security").map_err(orm_err)?;
    let email_address: String = row.try_get("", "email_address").map_err(orm_err)?;
    let auth_type: String = row.try_get("", "auth_type").map_err(orm_err)?;

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

    let oauth = crate::oauth::OAuthRegistry::refresh_configs();
    let secret = crate::oauth::resolve_mail_access_secret(
        db,
        account_id,
        &auth_type,
        &credential_json,
        &dek,
        Some(&host),
        &oauth,
    )
    .await
    .map_err(|e| {
        if e.is_credential_decrypt() {
            SyncError::Crypto(e.to_string())
        } else {
            SyncError::Protocol(e.to_string())
        }
    })?;

    let client = ImapClient::connect(&ImapConfig {
        host,
        port,
        security,
        username: email_address,
        password: zeroize::Zeroizing::new(secret.as_str().to_string()),
        xoauth2: secret.is_xoauth2(),
    })
    .await?;

    Ok((client, protocol))
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
    session: crate::auth::AuthSession,
    Query(query): Query<GetMessageQuery>,
) -> Result<Json<MessageResponse>, SyncError> {
    let db = state.db();
    let mut row = load_message_row(db, &session.user_id, &message_id).await?;
    maybe_fill_imap_body(db, &state.data_dir, &session.user_id, &mut row).await?;

    let allow_remote = query.remote_content.as_deref() == Some("allow");
    let response = finalize_message_response_with_opengpg(
        &state,
        &session.user_id,
        Some(&session.token),
        &row,
        allow_remote,
    )
    .await?;
    Ok(Json(response))
}

/// RHS for `SET col = COALESCE(col, <bound>)`; UPDATE statements carry no
/// aliases, so the bare column reference resolves locally.
fn coalesce_existing(column: message::Column, val: Value) -> sea_orm::sea_query::FunctionCall {
    Func::coalesce([Expr::col(column), Expr::val(val)])
}

/// Encoded-word sniffing RHS: refresh fields that are empty or still contain
/// `=?…?=` markers (the legacy `%=?%?=%` LIKE pattern).
fn refresh_if_stale(column: message::Column, replacement: Value) -> Expr {
    let stale = Condition::any()
        .add(Expr::col(column).is_null())
        .add(Expr::col(column).eq(""))
        .add(Expr::col(column).like("%=?%?=%"));
    Expr::case(stale, Expr::val(replacement))
        .finally(Expr::col(column))
        .into()
}

#[allow(clippy::too_many_lines)]
async fn maybe_fill_imap_body(
    db: &DbPool,
    data_dir: &std::path::Path,
    user_id: &str,
    row: &mut MessageRow,
) -> Result<(), SyncError> {
    let needs_body = row.body_text.is_none() && row.body_html.is_none();
    if !needs_body || row.protocol != "imap" {
        return Ok(());
    }

    if super::recovery::body_exceeds_limit(row.size_bytes) {
        tracing::warn!(
            message_id = %row.id,
            size_bytes = ?row.size_bytes,
            "skipping oversized message body fetch"
        );
        super::recovery::mark_message_fetch_error(db, &row.id, "message too large").await?;
        return Ok(());
    }

    let Ok(uid) = parse_imap_uid(row.external_id.as_deref()) else {
        return Ok(());
    };

    let (mut client, _) = connect_imap_for_account(db, user_id, &row.account_id).await?;
    client.select(&row.folder_name).await?;
    let bodies = client.fetch_bodies(&[uid]).await?;
    let Some(fetched) = bodies.into_iter().next() else {
        return Ok(());
    };

    if let Some(size) = fetched.size
        && u64::from(size) > super::recovery::MAX_MESSAGE_BODY_BYTES
    {
        tracing::warn!(
            message_id = %row.id,
            size,
            "skipping oversized IMAP body"
        );
        super::recovery::mark_message_fetch_error(db, &row.id, "message too large").await?;
        return Ok(());
    }

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
        .map(super::store::truncate_for_snippet)
        .or_else(|| {
            fetched
                .body_text
                .as_deref()
                .map(|t| t.chars().take(120).collect())
        });
    let body_html = persist_body_html(fetched.body_html.as_deref());

    let mut update = Sq::update();
    update
        .table(message::Entity)
        .value(
            message::Column::BodyText,
            owned_text_value(fetched.body_text.clone()),
        )
        .value(
            message::Column::BodyHtml,
            owned_text_value(body_html.clone()),
        )
        .value(message::Column::HasAttachments, fetched.has_attachments)
        .value(
            message::Column::Snippet,
            refresh_if_stale(message::Column::Snippet, owned_text_value(snippet.clone())),
        )
        .value(
            message::Column::Subject,
            refresh_if_stale(
                message::Column::Subject,
                text_value(fetched.subject.as_deref()),
            ),
        )
        .value(
            message::Column::FromAddress,
            coalesce_existing(
                message::Column::FromAddress,
                opt_json_value(db, from_json.as_deref()),
            ),
        )
        .value(
            message::Column::ToAddresses,
            coalesce_existing(
                message::Column::ToAddresses,
                opt_json_value(db, to_json.as_deref()),
            ),
        )
        .value(
            message::Column::Date,
            coalesce_existing(
                message::Column::Date,
                ts_value(db, fetched.date.as_deref().and_then(parse_ts)),
            ),
        )
        .value(
            message::Column::MessageIdHeader,
            coalesce_existing(
                message::Column::MessageIdHeader,
                text_value(fetched.message_id.as_deref()),
            ),
        )
        .value(message::Column::UpdatedAt, now_value(db))
        .and_where(Expr::col(message::Column::Id).eq(id_value(db, &row.id)?));
    db.orm().execute(&update).await.map_err(orm_err)?;

    row.body_text = fetched.body_text;
    row.body_html = body_html;
    row.has_attachments = fetched.has_attachments || !fetched.attachments.is_empty();
    if header_needs_refresh(row.subject.as_deref()) {
        row.subject.clone_from(&fetched.subject);
    }
    if row.from_address.is_none() {
        row.from_address = from_json;
    }
    if row.to_addresses.is_none() {
        row.to_addresses = to_json;
    }
    if row.date.is_none() {
        row.date.clone_from(&fetched.date);
    }
    if header_needs_refresh(row.snippet.as_deref()) {
        row.snippet = snippet;
    }
    persist_attachments(db, data_dir, &row.account_id, &row.id, &fetched.attachments).await?;
    Ok(())
}

/// PATCH /api/v1/messages/{id} — update read/starred flags (IMAP STORE when possible).
pub(crate) async fn patch_message(
    State(state): State<AuthState>,
    Path(message_id): Path<String>,
    session: crate::auth::AuthSession,
    Json(body): Json<PatchMessageRequest>,
) -> Result<Json<MessageResponse>, SyncError> {
    let db = state.db();
    let user_id = session.user_id.clone();
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

    let mut update = Sq::update();
    update
        .table(message::Entity)
        .value(message::Column::IsRead, next_read)
        .value(message::Column::IsStarred, next_star)
        .value(message::Column::UpdatedAt, now_value(db))
        .and_where(Expr::col(message::Column::Id).eq(id_value(db, &row.id)?));
    db.orm().execute(&update).await.map_err(orm_err)?;

    row.is_read = next_read;
    row.is_starred = next_star;

    // Keep folder unread counts roughly consistent.
    update_folder_counts(db, &row.folder_id).await?;

    Ok(Json(
        finalize_message_response_with_opengpg(&state, &user_id, Some(&session.token), &row, false)
            .await?,
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

/// POST /api/v1/messages/{id}/spam — move to Spam/Junk when available.
pub(crate) async fn spam_message(
    State(state): State<AuthState>,
    Path(message_id): Path<String>,
    AuthUser(user_id): AuthUser,
) -> Result<Json<serde_json::Value>, SyncError> {
    move_message_to_role(state, message_id, user_id, "spam").await
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

    let mut update = Sq::update();
    update
        .table(message::Entity)
        .value(message::Column::SnoozedUntil, ts_value(db, Some(until_utc)))
        .value(message::Column::UpdatedAt, now_value(db))
        .and_where(Expr::col(message::Column::Id).eq(id_value(db, &row.id)?));
    db.orm().execute(&update).await.map_err(orm_err)?;

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

#[allow(clippy::too_many_lines)]
pub(crate) async fn move_message_to_role(
    state: AuthState,
    message_id: String,
    user_id: String,
    role: &str,
) -> Result<Json<serde_json::Value>, SyncError> {
    let db = state.db();
    let row = load_message_row(db, &user_id, &message_id).await?;

    let account_value = id_value(db, &row.account_id)?;
    let dest = query_first(db, |q| {
        q.expr_as(Expr::col(folder::Column::Id), Alias::new("id"))
            .expr_as(
                Expr::col(folder::Column::ExternalId),
                Alias::new("external_id"),
            )
            .expr_as(Expr::col(folder::Column::Name), Alias::new("name"))
            .from(folder::Entity)
            .and_where(Expr::col(folder::Column::AccountId).eq(account_value))
            .and_where(Expr::cust_with_values(
                "COALESCE(role_override, role) = ?",
                [role],
            ))
            .order_by_expr(Expr::col(folder::Column::SortOrder), Order::Asc)
            .limit(1);
    })
    .await?;

    let Some(dest) = dest else {
        // No destination folder: soft-delete locally.
        let mut update = Sq::update();
        update
            .table(message::Entity)
            .value(message::Column::IsDeleted, true)
            .value(message::Column::UpdatedAt, now_value(db))
            .and_where(Expr::col(message::Column::Id).eq(id_value(db, &row.id)?));
        db.orm().execute(&update).await.map_err(orm_err)?;
        update_folder_counts(db, &row.folder_id).await?;
        return Ok(Json(serde_json::json!({
            "status": "ok",
            "action": "soft_delete",
            "role": role,
        })));
    };

    let dest_id = row_id(&dest, "id").map_err(orm_err)?;
    let external_id: Option<String> = dest.try_get("", "external_id").map_err(orm_err)?;
    let name: String = dest.try_get("", "name").map_err(orm_err)?;
    let dest_name = external_id.unwrap_or(name);

    if row.protocol == "imap" {
        let uid = parse_imap_uid(row.external_id.as_deref())?;
        let (mut client, _) = connect_imap_for_account(db, &user_id, &row.account_id).await?;
        client.select(&row.folder_name).await?;
        client.move_uid(uid, &dest_name).await?;
    }

    let mut move_update = Sq::update();
    move_update
        .table(message::Entity)
        .value(message::Column::FolderId, id_value(db, &dest_id)?)
        .value(message::Column::UpdatedAt, now_value(db))
        .and_where(Expr::col(message::Column::Id).eq(id_value(db, &row.id)?));
    db.orm().execute(&move_update).await.map_err(orm_err)?;

    update_folder_counts(db, &row.folder_id).await?;
    update_folder_counts(db, &dest_id).await?;

    Ok(Json(serde_json::json!({
        "status": "ok",
        "action": "moved",
        "role": role,
        "folderId": dest_id,
    })))
}
