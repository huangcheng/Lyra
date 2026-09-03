//! Read-side queries for the mail surface: dialect-aware value/row codecs,
//! shared message/folder SELECT builders, row→DTO mapping, and the
//! list/search/load query functions the HTTP handlers and sync loops use.
//!
//! Split from `http.rs` so handlers stay thin and the SQL seam has one home.

use chrono::{DateTime, Utc};
use sea_orm::sea_query::{
    Alias, Condition, Expr, ExprTrait, JoinType, Order, Query as Sq, SelectStatement,
};
use sea_orm::{ConnectionTrait, IdenStatic, QueryResult, Value};
use serde::Serialize;
use uuid::Uuid;

use crate::db_row::{IdParam, id_param};
use crate::entities::{folder, mail_account, message};
use crate::storage::DbPool;

use super::store::effective_folder_role;
use super::types::SyncError;

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
pub(super) fn orm_err(err: sea_orm::DbErr) -> SyncError {
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
pub(super) fn id_value(db: &DbPool, id: &str) -> Result<Value, SyncError> {
    Ok(match id_param(db, id)? {
        IdParam::Text(s) => Value::String(Some(s)),
        IdParam::Uuid(u) => Value::Uuid(Some(u)),
    })
}

/// Optional id bind (`InvalidIdError` still maps to 400 on Postgres).
pub(super) fn opt_id_value(db: &DbPool, id: Option<&str>) -> Result<Option<Value>, SyncError> {
    id.map(|s| id_value(db, s)).transpose()
}

/// Plain text value (`None` becomes a typed NULL).
pub(super) fn text_value(raw: Option<&str>) -> Value {
    Value::String(raw.map(str::to_owned))
}

/// Owned-string variant of [`text_value`].
pub(super) fn owned_text_value(raw: Option<String>) -> Value {
    Value::String(raw)
}

/// Typed JSON NULL matching the dialect (TEXT vs JSONB).
pub(super) fn json_null_value(db: &DbPool) -> Value {
    match db {
        DbPool::Sqlite(_) => Value::String(None),
        #[cfg(feature = "postgres")]
        DbPool::Postgres(_) => Value::Json(None),
    }
}

/// Bind optional JSON text for JSONB/TEXT columns (lenient like the macro
/// layer's [`crate::db_row::JsonParam`]: non-JSON strings stay raw on SQLite
/// and become string scalars on Postgres).
pub(super) fn opt_json_value(db: &DbPool, raw: Option<&str>) -> Value {
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
pub(super) fn ts_value(db: &DbPool, dt: Option<DateTime<Utc>>) -> Value {
    match db {
        DbPool::Sqlite(_) => Value::String(dt.map(|d| d.format("%Y-%m-%d %H:%M:%S").to_string())),
        #[cfg(feature = "postgres")]
        DbPool::Postgres(_) => Value::ChronoDateTimeUtc(dt),
    }
}

/// `updated_at` write, shaped like the legacy `datetime('now')` / `NOW()`
/// defaults so sqlite rows keep their `YYYY-MM-DD HH:MM:SS` text format.
pub(super) fn now_value(db: &DbPool) -> Value {
    match db {
        DbPool::Sqlite(_) => {
            Value::String(Some(Utc::now().format("%Y-%m-%d %H:%M:%S").to_string()))
        }
        #[cfg(feature = "postgres")]
        DbPool::Postgres(_) => Value::ChronoDateTimeUtc(Some(Utc::now())),
    }
}

pub(super) fn missing_column(col: &str) -> sea_orm::DbErr {
    sea_orm::DbErr::Query(sea_orm::RuntimeErr::Internal(format!(
        "missing column {col}"
    )))
}

/// Decode a UUID/TEXT id column: `String` on SQLite, native UUID on Postgres.
pub(super) fn row_id(row: &QueryResult, col: &str) -> Result<String, sea_orm::DbErr> {
    if let Some(s) = row.try_get::<Option<String>>("", col).ok().flatten() {
        return Ok(s);
    }
    row.try_get::<Option<Uuid>>("", col)?
        .map(|u| u.to_string())
        .ok_or_else(|| missing_column(col))
}

/// Nullable id column ([`row_id`] semantics).
pub(super) fn row_opt_id(row: &QueryResult, col: &str) -> Result<Option<String>, sea_orm::DbErr> {
    if let Ok(text) = row.try_get::<Option<String>>("", col) {
        return Ok(text);
    }
    Ok(row.try_get::<Option<Uuid>>("", col)?.map(|u| u.to_string()))
}

/// Nullable timestamp column: stored text on SQLite, RFC3339 on Postgres.
pub(super) fn row_opt_ts(row: &QueryResult, col: &str) -> Result<Option<String>, sea_orm::DbErr> {
    if let Ok(text) = row.try_get::<Option<String>>("", col) {
        return Ok(text.map(crate::db_row::normalize_ts_text));
    }
    row.try_get::<Option<DateTime<Utc>>>("", col)
        .map(|opt| opt.map(|t| t.to_rfc3339()))
}

/// JSONB / TEXT json → JSON text for the API (`from_address`, …).
///
/// Stored TEXT is returned verbatim when present (SQLite keeps raw header
/// text too); Postgres falls back to native JSONB decode.
pub(super) fn row_json_text(
    row: &QueryResult,
    col: &str,
) -> Result<Option<String>, sea_orm::DbErr> {
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
    message::Column::InReplyTo,
    message::Column::ReferencesHeaders,
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
    message::Column::IsDraft,
    message::Column::HasAttachments,
];

pub(super) fn add_message_list_columns(query: &mut SelectStatement) {
    for col in MESSAGE_LIST_COLS {
        query.expr_as(m_col(*col), Alias::new(col.as_str()));
    }
    // Folder role with override: derives `is_draft` (and future role-driven
    // display state). Every caller must also join `folder AS f`.
    query.expr_as(
        Expr::cust("COALESCE(f.role_override, f.role)"),
        Alias::new("folder_role"),
    );
}

/// `FROM message AS m JOIN mail_account AS a ON m.account_id = a.id`.
pub(super) fn add_message_account_join(query: &mut SelectStatement) {
    query.from_as(message::Entity, Alias::new("m")).join_as(
        JoinType::InnerJoin,
        mail_account::Entity,
        Alias::new("a"),
        Expr::cust("m.account_id = a.id"),
    );
}

/// Additionally `JOIN folder AS f ON m.folder_id = f.id`.
pub(super) fn add_message_folder_join(query: &mut SelectStatement) {
    query.join_as(
        JoinType::InnerJoin,
        folder::Entity,
        Alias::new("f"),
        Expr::cust("m.folder_id = f.id"),
    );
}

/// Soft-deleted filter, bound through the entity-shaped column name.
/// Backend-correct `alias.column` reference for equality/range conditions.
///
/// Raw `?` placeholders via `Expr::cust_with_values` are SQLite-only:
/// Postgres prepared statements require `$N`, which sea_query emits only
/// for typed conditions. Compose as `aliased_col("a", "user_id").eq(v)`.
pub(super) fn aliased_col(alias: &str, column: &str) -> Expr {
    Expr::cust(format!("{alias}.{column}"))
}

pub(super) fn not_deleted_clause() -> Expr {
    aliased_col("m", "is_deleted").eq(Expr::val(false))
}

/// Snooze visibility kept explicitly dialect-branched: SQLite compares
/// `snoozed_until` text against `datetime('now')`; Postgres compares
/// TIMESTAMPTZ against NOW().
const SNOOZE_VISIBLE_SQLITE: &str =
    "(m.snoozed_until IS NULL OR m.snoozed_until <= datetime('now'))";

#[cfg(feature = "postgres")]
const SNOOZE_VISIBLE_POSTGRES: &str = "(m.snoozed_until IS NULL OR m.snoozed_until <= NOW())";

pub(super) fn snooze_visible_clause(db: &DbPool) -> Expr {
    match db {
        DbPool::Sqlite(_) => Expr::cust(SNOOZE_VISIBLE_SQLITE),
        #[cfg(feature = "postgres")]
        DbPool::Postgres(_) => Expr::cust(SNOOZE_VISIBLE_POSTGRES),
    }
}

pub(super) fn message_response_from_query_row(
    row: &QueryResult,
) -> Result<MessageResponse, sea_orm::DbErr> {
    Ok(MessageResponse {
        id: row_id(row, "id")?,
        account_id: row_id(row, "account_id")?,
        folder_id: row_id(row, "folder_id")?,
        message_id_header: row.try_get("", "message_id_header")?,
        in_reply_to: row.try_get("", "in_reply_to")?,
        references_headers: row
            .try_get::<Option<String>>("", "references_headers")?
            .map(|s| s.chars().take(2048).collect()),
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
        is_draft: {
            let stored: bool = row.try_get("", "is_draft")?;
            let folder_role: Option<String> = row.try_get("", "folder_role")?;
            stored || folder_role.as_deref() == Some("drafts")
        },
        has_attachments: row.try_get("", "has_attachments")?,
        remote_content_blocked: false,
        opengpg: None,
        attachments: None,
        dkim: None,
    })
}

/// Build + run one of the listing queries and map rows to the API DTO.
pub(super) async fn run_message_list(
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
pub(super) async fn query_first(
    db: &DbPool,
    build: impl FnOnce(&mut SelectStatement),
) -> Result<Option<QueryResult>, SyncError> {
    let mut query = Sq::select();
    build(&mut query);
    db.orm().query_one(&query).await.map_err(orm_err)
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

/// DKIM verdict in the detail payload (`null` = never verified).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DkimResponse {
    pub status: String,
    pub sdid: Option<String>,
    pub auid: Option<String>,
    pub selector: Option<String>,
    pub algorithm: Option<String>,
    pub signed_headers: Vec<String>,
    pub warnings: Vec<String>,
    pub signed_at: Option<String>,
    pub expires_at: Option<String>,
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
    /// copies of the same message (e.g. INBOX + Archive) and to link
    /// replies via In-Reply-To/References into threads.
    pub message_id_header: Option<String>,
    /// RFC 5322 In-Reply-To — direct parent Message-ID(s).
    pub in_reply_to: Option<String>,
    /// RFC 5322 References — ancestor Message-ID chain (capped to bound
    /// list payload weight).
    pub references_headers: Option<String>,
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
    pub is_draft: bool,
    pub has_attachments: bool,
    /// True when remote images were replaced with placeholders in this response.
    pub remote_content_blocked: bool,
    /// OpenGPG decrypt/verify status when the message looks encrypted or signed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub opengpg: Option<crate::opengpg::OpengpgMessageStatus>,
    /// Attachment metadata; set by the detail endpoint only (lists carry
    /// `has_attachments` instead).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attachments: Option<Vec<AttachmentResponse>>,
    /// DKIM verdict; detail endpoint only, absent when never verified.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dkim: Option<DkimResponse>,
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

pub(super) fn add_folder_columns(query: &mut SelectStatement) {
    for col in FOLDER_COLS {
        query.expr_as(
            Expr::col((Alias::new("f"), Alias::new(col.as_str()))),
            Alias::new(col.as_str()),
        );
    }
}

pub(super) fn folder_response_from_row(
    row: &QueryResult,
) -> Result<FolderResponse, sea_orm::DbErr> {
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

/// List messages for a user, optionally filtered by folder role, account, and/or starred.
///
/// Optional filters become conditional `WHERE`s instead of the legacy
/// `(? IS NULL OR … = ?)` duality — the resulting predicate set is identical.
pub(crate) async fn query_user_messages(
    db: &DbPool,
    user_id: &str,
    role: Option<&str>,
    account_id: Option<&str>,
    is_starred: Option<bool>,
) -> Result<Vec<MessageResponse>, SyncError> {
    let user_value = id_value(db, user_id)?;
    let account_value = opt_id_value(db, account_id)?;

    run_message_list(db, |q| {
        add_message_account_join(q);
        add_message_folder_join(q);
        q.and_where(aliased_col("a", "user_id").eq(Expr::val(user_value)))
            .and_where(not_deleted_clause())
            .and_where(snooze_visible_clause(db));
        if let Some(role) = role {
            q.and_where(Expr::cust("COALESCE(f.role_override, f.role)").eq(Expr::val(role)));
        }
        if let Some(account_value) = account_value {
            q.and_where(aliased_col("m", "account_id").eq(Expr::val(account_value)));
        }
        if let Some(starred) = is_starred {
            q.and_where(aliased_col("m", "is_starred").eq(Expr::val(starred)));
        }
        q.order_by_expr(Expr::cust("m.date"), Order::Desc)
            .limit(500);
    })
    .await
}

/// LIKE fallback branch (used only when the FTS index is unavailable).
///
/// The keyword spelling follows the legacy rewrite exactly (SQLite `LIKE`,
/// already case-insensitive for ASCII; Postgres `ILIKE`), so the templates are
/// static strings chosen by dialect; the pattern binds through the trailing
/// placeholder.
pub(super) async fn search_like_fallback(
    db: &DbPool,
    q: &str,
    user_value: Value,
    account_value: Option<Value>,
    folder_id: Option<&str>,
    limit: i64,
) -> Result<Vec<MessageResponse>, SyncError> {
    let use_ilike = matches!(db, DbPool::Postgres(_));
    let columns = ["m.subject", "m.snippet", "m.body_text", "m.from_address"];

    let folder_value = opt_id_value(db, folder_id)?;
    let pattern = format!("%{q}%");

    let mut any = Condition::any();
    for col in columns {
        let op = if use_ilike { "ILIKE" } else { "LIKE" };
        any =
            any.add(Expr::cust(format!("COALESCE({col}, '') {op}")).eq(Expr::val(pattern.clone())));
    }

    run_message_list(db, |sel| {
        add_message_account_join(sel);
        add_message_folder_join(sel);
        sel.and_where(aliased_col("a", "user_id").eq(Expr::val(user_value)))
            .and_where(not_deleted_clause());
        if let Some(account_value) = account_value {
            sel.and_where(aliased_col("m", "account_id").eq(Expr::val(account_value)));
        }
        if let Some(folder_value) = folder_value {
            sel.and_where(aliased_col("m", "folder_id").eq(Expr::val(folder_value)));
        }
        sel.cond_where(any)
            .order_by_expr(Expr::cust("m.date"), Order::Desc)
            .limit(u64::try_from(limit).unwrap_or(500));
    })
    .await
}

/// Fetch FTS hits (in rank order), mapping each to a response the requesting
/// user owns and that is not soft-deleted.
pub(super) async fn fetch_messages_by_ids(
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
            add_message_folder_join(q);
            q.and_where(aliased_col("m", "id").eq(Expr::val(msg_value.clone())))
                .and_where(aliased_col("a", "user_id").eq(Expr::val(user_value.clone())))
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
    pub(super) id: String,
    pub(super) message_id: String,
    pub(super) filename: Option<String>,
    pub(super) content_type: Option<String>,
    pub(super) size_bytes: Option<i64>,
    pub(super) is_inline: bool,
    /// CID for inline parts (`<image.png@…>` in HTML `src="cid:…"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_id: Option<String>,
}

/// Loaded message row with account/folder context for mutations.
// Schema-mapped row: bool columns mirror the DB 1:1.
#[allow(clippy::struct_excessive_bools)]
pub(crate) struct MessageRow {
    pub(super) id: String,
    pub(super) account_id: String,
    pub(super) folder_id: String,
    pub(super) folder_name: String,
    pub(super) external_id: Option<String>,
    pub(super) message_id_header: Option<String>,
    pub(super) in_reply_to: Option<String>,
    pub(super) references_headers: Option<String>,
    pub(super) protocol: String,
    pub(super) body_text: Option<String>,
    pub(super) body_html: Option<String>,
    pub(super) is_read: bool,
    pub(super) is_starred: bool,
    pub(super) is_draft: bool,
    pub(super) folder_role: Option<String>,
    pub(super) subject: Option<String>,
    pub(super) from_address: Option<String>,
    pub(super) to_addresses: Option<String>,
    pub(super) cc_addresses: Option<String>,
    pub(super) date: Option<String>,
    pub(super) snippet: Option<String>,
    pub(super) has_attachments: bool,
    pub(super) size_bytes: Option<i64>,
    pub(super) dkim_status: Option<String>,
    pub(super) dkim_sdid: Option<String>,
    pub(super) dkim_auid: Option<String>,
    pub(super) dkim_selector: Option<String>,
    pub(super) dkim_algorithm: Option<String>,
    pub(super) dkim_signed_headers: Option<String>,
    pub(super) dkim_warnings: Option<String>,
    pub(super) dkim_signed_at: Option<String>,
    pub(super) dkim_expires_at: Option<String>,
}

pub(super) fn dkim_response_from_row(row: &MessageRow) -> Option<DkimResponse> {
    let status = row.dkim_status.clone()?;
    Some(DkimResponse {
        status,
        sdid: row.dkim_sdid.clone(),
        auid: row.dkim_auid.clone(),
        selector: row.dkim_selector.clone(),
        algorithm: row.dkim_algorithm.clone(),
        signed_headers: serde_json::from_str(row.dkim_signed_headers.as_deref().unwrap_or("[]"))
            .unwrap_or_default(),
        warnings: serde_json::from_str(row.dkim_warnings.as_deref().unwrap_or("[]"))
            .unwrap_or_default(),
        signed_at: row.dkim_signed_at.clone(),
        expires_at: row.dkim_expires_at.clone(),
    })
}

pub(crate) fn message_response_from_row(row: &MessageRow) -> MessageResponse {
    MessageResponse {
        id: row.id.clone(),
        account_id: row.account_id.clone(),
        folder_id: row.folder_id.clone(),
        message_id_header: row.message_id_header.clone(),
        in_reply_to: row.in_reply_to.clone(),
        references_headers: row
            .references_headers
            .as_deref()
            .map(|s| s.chars().take(2048).collect()),
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
        is_draft: row.is_draft || row.folder_role.as_deref() == Some("drafts"),
        has_attachments: row.has_attachments,
        remote_content_blocked: false,
        opengpg: None,
        attachments: None,
        dkim: dkim_response_from_row(row),
    }
}

pub(super) fn header_needs_refresh(existing: Option<&str>) -> bool {
    let value = existing.unwrap_or("");
    value.is_empty() || (value.contains("=?") && value.contains("?=")) || value.contains('\u{FFFD}')
}

/// Message columns loaded by [`load_message_row`] (plus `f.external_id AS
/// folder_name` and `a.protocol` joined below).
const MESSAGE_LOAD_COLS: &[message::Column] = &[
    message::Column::Id,
    message::Column::AccountId,
    message::Column::FolderId,
    message::Column::ExternalId,
    message::Column::MessageIdHeader,
    message::Column::InReplyTo,
    message::Column::ReferencesHeaders,
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
    message::Column::IsDraft,
    message::Column::HasAttachments,
    message::Column::SizeBytes,
    message::Column::DkimStatus,
    message::Column::DkimSdid,
    message::Column::DkimAuid,
    message::Column::DkimSelector,
    message::Column::DkimAlgorithm,
    message::Column::DkimSignedHeaders,
    message::Column::DkimWarnings,
    message::Column::DkimSignedAt,
    message::Column::DkimExpiresAt,
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
        .expr_as(
            Expr::cust("COALESCE(f.role_override, f.role)"),
            Alias::new("folder_role"),
        )
        .expr_as(Expr::cust("a.protocol"), Alias::new("protocol"));
    add_message_account_join(&mut query);
    add_message_folder_join(&mut query);
    query
        .and_where(aliased_col("m", "id").eq(Expr::val(msg_value)))
        .and_where(aliased_col("a", "user_id").eq(Expr::val(user_value)))
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
        in_reply_to: row.try_get("", "in_reply_to").map_err(orm_err)?,
        references_headers: row.try_get("", "references_headers").map_err(orm_err)?,
        protocol: row.try_get("", "protocol").map_err(orm_err)?,
        body_text: row.try_get("", "body_text").map_err(orm_err)?,
        body_html: row.try_get("", "body_html").map_err(orm_err)?,
        is_read: row.try_get("", "is_read").map_err(orm_err)?,
        is_starred: row.try_get("", "is_starred").map_err(orm_err)?,
        is_draft: row.try_get("", "is_draft").map_err(orm_err)?,
        folder_role: row.try_get("", "folder_role").map_err(orm_err)?,
        subject: row.try_get("", "subject").map_err(orm_err)?,
        from_address: row_json_text(&row, "from_address").map_err(orm_err)?,
        to_addresses: row_json_text(&row, "to_addresses").map_err(orm_err)?,
        cc_addresses: row_json_text(&row, "cc_addresses").map_err(orm_err)?,
        date: row_opt_ts(&row, "date").map_err(orm_err)?,
        snippet: row.try_get("", "snippet").map_err(orm_err)?,
        has_attachments: row.try_get("", "has_attachments").map_err(orm_err)?,
        size_bytes: row.try_get("", "size_bytes").map_err(orm_err)?,
        dkim_status: row.try_get("", "dkim_status").map_err(orm_err)?,
        dkim_sdid: row.try_get("", "dkim_sdid").map_err(orm_err)?,
        dkim_auid: row.try_get("", "dkim_auid").map_err(orm_err)?,
        dkim_selector: row.try_get("", "dkim_selector").map_err(orm_err)?,
        dkim_algorithm: row.try_get("", "dkim_algorithm").map_err(orm_err)?,
        dkim_signed_headers: row.try_get("", "dkim_signed_headers").map_err(orm_err)?,
        dkim_warnings: row.try_get("", "dkim_warnings").map_err(orm_err)?,
        dkim_signed_at: row_opt_ts(&row, "dkim_signed_at").map_err(orm_err)?,
        dkim_expires_at: row_opt_ts(&row, "dkim_expires_at").map_err(orm_err)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::sea_query::{PostgresQueryBuilder, Query as Sq, SqliteQueryBuilder};

    /// Regression: raw `?` placeholders (`Expr::cust_with_values`) are
    /// SQLite-only — Postgres requires `$N`, which sea_query emits only for
    /// typed conditions (`aliased_col(..).eq(..)`). This broke every
    /// message/folder/stats read on the Postgres deployment while SQLite
    /// (and the SQLite test suite) stayed green.
    #[test]
    fn typed_conditions_render_per_backend_placeholders() {
        let uid = uuid::Uuid::nil();
        let mut q = Sq::select();
        q.expr_as(aliased_col("m", "id"), Alias::new("id"));
        q.from_as(message::Entity, Alias::new("m"));
        q.and_where(aliased_col("a", "user_id").eq(Value::Uuid(Some(uid))));
        q.and_where(aliased_col("m", "is_deleted").eq(Expr::val(false)));
        q.and_where(aliased_col("m", "account_id").eq("acc-1"));
        q.order_by_expr(Expr::cust("m.date"), Order::Desc);
        q.limit(500);

        let pg = q.to_string(PostgresQueryBuilder);
        // sea-query 1.x folds typed values to backend-safe literals (or
        // numbered params); either way a bare `?` must never reach Postgres.
        assert!(
            !pg.as_str().contains('?'),
            "raw ? is invalid postgres syntax: {pg}"
        );
        assert!(
            pg.as_str().contains("= '00000000-"),
            "value must be bound or folded: {pg}"
        );
        assert!(pg.as_str().contains("= FALSE"), "bool folded: {pg}");

        let sqlite = q.to_string(SqliteQueryBuilder);
        assert!(
            !sqlite.as_str().contains("= ?")
                || sqlite.as_str().matches('?').count() <= pg.as_str().matches('?').count()
        );
    }
}

#[cfg(test)]
#[cfg(feature = "postgres")]
mod postgres_live {
    //! Read-path roundtrips: the API-facing message list + single-message
    //! load must survive PostgreSQL typing (uuid binds, TEXT timestamps,
    //! jsonb reads). See `pgtest` for the harness contract.

    use crate::pgtest::support;
    use crate::sync::store;

    #[test]
    #[ignore = "needs postgres"]
    fn message_list_and_row_roundtrip() {
        support::rt().block_on(async {
            let (db, user_id) = support::setup().await;
            let account_id = support::seed_account(&db, &user_id, "queries@example.com").await;
            let folder_id = support::seed_inbox(&db, &account_id).await;
            store::upsert_message(
                &db,
                &account_id,
                &folder_id,
                &support::message(11, "Query me on postgres", "q@example.com"),
            )
            .await
            .unwrap();

            let list = super::query_user_messages(&db, &user_id, Some("inbox"), None, None)
                .await
                .unwrap();
            let hit = list
                .iter()
                .find(|m| m.subject.as_deref() == Some("Query me on postgres"))
                .expect("seeded message is listed for its user");

            let row = super::load_message_row(&db, &user_id, &hit.id)
                .await
                .unwrap();
            assert_eq!(row.subject.as_deref(), Some("Query me on postgres"));
            assert_eq!(row.folder_name, "INBOX");
            assert_eq!(row.protocol, "imap");

            // Ownership: another user's id sees nothing.
            let other = "00000000-0000-7000-8000-000000000000";
            assert!(
                super::load_message_row(&db, other, &hit.id).await.is_err(),
                "cross-user message load must fail"
            );
        });
    }
}
