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
use sea_orm::sea_query::{Alias, Condition, Expr, ExprTrait, Func, JoinType, Order, Query as Sq};
use sea_orm::{
    ColumnTrait, ConnectionTrait, EntityTrait, IdenStatic, QueryFilter, QuerySelect, Value,
};
use serde::Deserialize;
use uuid::Uuid;

use super::queries::{
    AttachmentResponse, FolderResponse, MessageResponse, MessageRow, add_folder_columns,
    add_message_account_join, add_message_folder_join, aliased_col, fetch_messages_by_ids,
    folder_response_from_row, header_needs_refresh, id_value, load_message_row,
    message_response_from_row, not_deleted_clause, now_value, opt_id_value, opt_json_value,
    orm_err, owned_text_value, query_first, query_user_messages, row_id, row_json_text,
    run_message_list, search_like_fallback, snooze_visible_clause, text_value, ts_value,
};
use super::send::send_message;
use super::store::{opt_str_value, parse_imap_uid, update_folder_counts};
use super::types::{EnqueuedSync, SyncError, SyncStatus};
use crate::auth::{AuthState, AuthUser};
use crate::db_row::parse_ts;
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
        .route("/api/v1/drafts", post(save_draft))
        .route(
            "/api/v1/messages/{message_id}/draft",
            axum::routing::delete(discard_draft),
        )
        .route("/api/v1/messages/{message_id}/move", post(move_message))
        .route("/api/v1/messages/{message_id}/copy", post(copy_message))
        .route("/api/v1/messages/{message_id}/trash", post(trash_message))
        .route(
            "/api/v1/messages/{message_id}/archive",
            post(archive_message),
        )
        .route("/api/v1/messages/{message_id}/spam", post(spam_message))
        .route("/api/v1/messages/{message_id}/snooze", post(snooze_message))
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
        .and_where(aliased_col("a", "user_id").eq(Expr::val(user_value)))
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
        .and_where(aliased_col("f", "id").eq(Expr::val(folder_value.clone())))
        .and_where(aliased_col("a", "user_id").eq(Expr::val(user_value)));
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
            .and_where(Expr::col(folder::Column::Id).eq(Expr::val(folder_value.clone())));
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
            .and_where(Expr::col(folder::Column::Id).eq(Expr::val(folder_value.clone())));
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
        .and_where(aliased_col("f", "id").eq(Expr::val(folder_value)));

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
        .and_where(Expr::col(folder::Column::Id).eq(Expr::val(folder_value)));
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
        .and_where(aliased_col("f", "id").eq(Expr::val(folder_value.clone())))
        .and_where(aliased_col("a", "user_id").eq(Expr::val(user_value)));
    })
    .await?;
    if check.is_none() {
        return Err(SyncError::AccountNotFound);
    }

    let messages = run_message_list(db, |q| {
        q.from_as(message::Entity, Alias::new("m"));
        add_message_folder_join(q);
        q.and_where(aliased_col("m", "folder_id").eq(Expr::val(folder_value)))
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

pub(crate) async fn list_attachments(
    State(state): State<AuthState>,
    Path(message_id): Path<String>,
    AuthUser(user_id): AuthUser,
) -> Result<Json<Vec<AttachmentResponse>>, SyncError> {
    let db = state.db();
    let _ = load_message_row(db, &user_id, &message_id).await?;
    Ok(Json(load_attachment_meta(db, &message_id).await?))
}

/// Attachment metadata for one message, oldest first.
pub(crate) async fn load_attachment_meta(
    db: &DbPool,
    message_id: &str,
) -> Result<Vec<AttachmentResponse>, SyncError> {
    let message_value = id_value(db, message_id)?;

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
                attachment::Column::ContentId,
            ])
            .from(attachment::Entity)
            .and_where(Expr::col(attachment::Column::MessageId).eq(message_value))
            .order_by_expr(Expr::col(attachment::Column::CreatedAt), Order::Asc);
            q
        })
        .await
        .map_err(orm_err)?;

    rows.iter()
        .map(|row| {
            Ok(AttachmentResponse {
                id: row_id(row, "id")?,
                message_id: row_id(row, "message_id")?,
                filename: row.try_get("", "filename")?,
                content_type: row.try_get("", "content_type")?,
                size_bytes: row.try_get("", "size_bytes")?,
                is_inline: row.try_get("", "is_inline")?,
                content_id: row.try_get("", "content_id")?,
            })
        })
        .collect::<Result<Vec<_>, sea_orm::DbErr>>()
        .map_err(orm_err)
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
        .and_where(aliased_col("a", "id").eq(Expr::val(att_value)))
        .and_where(aliased_col("acc", "user_id").eq(Expr::val(user_value)));
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
        .and_where(Expr::col(mail_account::Column::UserId).eq(Expr::val(user_value)))
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

/// Connect the cached JMAP seam for an account (discovers on cache miss).
pub(crate) async fn connect_jmap_for_account(
    db: &DbPool,
    user_id: &str,
    account_id: &str,
) -> Result<std::sync::Arc<crate::sync::jmap_client::JmapSeam>, SyncError> {
    let (dek, credential_json) =
        crate::auth::AuthState::get_user_dek_and_credential(db, user_id, account_id)
            .await
            .map_err(|e| SyncError::Crypto(e.to_string()))?;
    let acct_value = id_value(db, account_id)?;
    let user_value = id_value(db, user_id)?;

    let row = query_first(db, |q| {
        q.expr_as(
            Expr::col(mail_account::Column::JmapBaseUrl),
            Alias::new("jmap_base_url"),
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
        .and_where(Expr::col(mail_account::Column::UserId).eq(Expr::val(user_value)))
        .and_where(Expr::col(mail_account::Column::IsActive).eq(true));
    })
    .await?
    .ok_or(SyncError::AccountNotFound)?;

    let jmap_base_url: Option<String> = row.try_get("", "jmap_base_url").map_err(orm_err)?;
    let email_address: String = row.try_get("", "email_address").map_err(orm_err)?;
    let auth_type: String = row.try_get("", "auth_type").map_err(orm_err)?;
    let base_url = jmap_base_url
        .ok_or_else(|| SyncError::InvalidInput("JMAP base URL not configured".into()))?;
    let password = crate::sync::jmap_client::decrypt_account_password(&credential_json, &dek)?;
    Ok(crate::sync::jmap_client::JmapSeam::connect_for_account(
        account_id,
        &base_url,
        &email_address,
        &password,
        &auth_type,
    )
    .await?)
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
    let fill_verified =
        maybe_fill_imap_body(db, &state.data_dir, &session.user_id, &mut row).await?;
    maybe_verify_dkim(db, state.kv(), &session.user_id, &mut row, fill_verified).await;

    let allow_remote = query.remote_content.as_deref() == Some("allow");
    let mut response = finalize_message_response_with_opengpg(
        &state,
        &session.user_id,
        Some(&session.token),
        &row,
        allow_remote,
    )
    .await?;
    // Detail payload carries attachment metadata (lists use has_attachments).
    response.attachments = Some(load_attachment_meta(db, &message_id).await?);
    Ok(Json(response))
}

/// Lowercased domain of the message's primary From address ("" when absent).
fn from_domain_of(row: &MessageRow) -> String {
    crate::privacy::sender_email_from_json(row.from_address.as_deref())
        .and_then(|addr| addr.rsplit('@').next().map(str::to_ascii_lowercase))
        .unwrap_or_default()
}

/// How long a `temperror` DKIM verdict waits before its next lazy retry
/// (full-body refetch + DNS). Short enough to heal after a resolver blip,
/// long enough that a hard outage doesn't refetch on every open.
const DKIM_TEMPERROR_RETRY_BACKOFF_SECS: u64 = 60 * 60;

/// Lazy DKIM for rows without a verdict: refetch raw bytes once, verify,
/// store, and reflect into the row. Failure-safe: on any error the message
/// serves without a verdict and the row stays NULL for a later retry.
/// `fill_verified` is true when the body-fill hook already ran verification
/// this same request — its verdict (even `temperror`) is fresh, so skip the
/// lazy refetch instead of verifying twice per open.
///
/// `temperror` retries carry a kv backoff: re-verifying requires the full
/// raw body again (IMAP/JMAP fetch + the 5s DNS budget), so a resolver
/// outage must not turn every open into a refetch. Backoff is keyed per
/// message and only set when the retry again lands on `temperror`.
async fn maybe_verify_dkim(
    db: &DbPool,
    kv: &std::sync::Arc<dyn crate::kv::KvStore>,
    user_id: &str,
    row: &mut MessageRow,
    fill_verified: bool,
) {
    let needs = row.dkim_status.is_none() || row.dkim_status.as_deref() == Some("temperror");
    if fill_verified || !needs || super::recovery::body_exceeds_limit(row.size_bytes) {
        return;
    }
    let backoff_key = format!("dkim:retry-backoff:{}", row.id);
    let is_retry = row.dkim_status.is_some();
    if is_retry && matches!(kv.get(&backoff_key).await, Ok(Some(_))) {
        return;
    }
    let raw: Option<Vec<u8>> = match row.protocol.as_str() {
        "imap" => {
            let Ok(uid) = parse_imap_uid(row.external_id.as_deref()) else {
                return;
            };
            let Ok((mut client, _)) = connect_imap_for_account(db, user_id, &row.account_id).await
            else {
                return;
            };
            if client.select(&row.folder_name).await.is_err() {
                return;
            }
            client
                .fetch_bodies(&[uid])
                .await
                .ok()
                .and_then(|b| b.into_iter().next())
                .and_then(|m| m.body)
        }
        "jmap" => {
            let Some(email_id) = row.external_id.clone() else {
                return;
            };
            let Ok(seam) = connect_jmap_for_account(db, user_id, &row.account_id).await else {
                return;
            };
            let blob_id = seam
                .get_emails(&[email_id])
                .await
                .ok()
                .and_then(|(emails, _)| emails.into_iter().next())
                .and_then(|e| e.blob_id);
            match blob_id {
                Some(id) => seam.download_blob(&id).await.ok(),
                None => None,
            }
        }
        _ => None,
    };
    let Some(raw) = raw else { return };
    let verdict = crate::dkim::verify_raw(&raw, &from_domain_of(row)).await;
    if verdict.status == crate::dkim::DkimStatus::TempError {
        // Only retriable verdicts back off; pass/fail persist and never
        // re-enter the lazy path.
        let _ = kv
            .set(&backoff_key, "1", Some(DKIM_TEMPERROR_RETRY_BACKOFF_SECS))
            .await;
    }
    if crate::sync::store::update_dkim_verdict(db, &row.id, &verdict)
        .await
        .is_ok()
    {
        row.dkim_status = Some(verdict.status.as_str().to_string());
        row.dkim_sdid.clone_from(&verdict.sdid);
        row.dkim_auid.clone_from(&verdict.auid);
        row.dkim_selector.clone_from(&verdict.selector);
        row.dkim_algorithm.clone_from(&verdict.algorithm);
        row.dkim_signed_headers =
            Some(serde_json::to_string(&verdict.signed_headers).unwrap_or_default());
        row.dkim_warnings = Some(serde_json::to_string(&verdict.warnings).unwrap_or_default());
        row.dkim_signed_at = verdict.signed_at.map(|d| d.to_rfc3339());
        row.dkim_expires_at = verdict.expires_at.map(|d| d.to_rfc3339());
    }
}

/// RHS for `SET col = COALESCE(col, <bound>)`; UPDATE statements carry no
/// aliases, so the bare column reference resolves locally.
fn coalesce_existing(column: message::Column, val: Value) -> sea_orm::sea_query::FunctionCall {
    Func::coalesce([Expr::col(column), Expr::val(val)])
}

/// Encoded-word sniffing RHS: refresh fields that are empty, still contain
/// `=?…?=` markers (the legacy `%=?%?=%` LIKE pattern), or carry U+FFFD
/// mojibake from before full legacy-charset decoding.
///
/// The stale checks run against `CAST(col AS text)`: on Postgres the address
/// columns are JSONB, so a bare `col = ''` / `col LIKE …` raises
/// `operator does not exist: jsonb = text`. The cast is a no-op on SQLite.
fn refresh_if_stale(column: message::Column, replacement: Value) -> Expr {
    let as_text = Expr::col(column).cast_as(Alias::new("text"));
    let stale = Condition::any()
        .add(Expr::col(column).is_null())
        .add(as_text.clone().eq(""))
        .add(as_text.clone().like("%=?%?=%"))
        .add(as_text.like("%\u{FFFD}%"));
    Expr::case(stale, Expr::val(replacement))
        .finally(Expr::col(column))
        .into()
}

/// Lazily fetch and persist the IMAP body for a message row. Returns `true`
/// when DKIM verification ran on the fetched raw bytes during this call, so
/// the caller can skip the lazy verify path.
#[allow(clippy::too_many_lines)]
async fn maybe_fill_imap_body(
    db: &DbPool,
    data_dir: &std::path::Path,
    user_id: &str,
    row: &mut MessageRow,
) -> Result<bool, SyncError> {
    let needs_body = row.body_text.is_none() && row.body_html.is_none();
    if !needs_body || row.protocol != "imap" {
        return Ok(false);
    }

    if super::recovery::body_exceeds_limit(row.size_bytes) {
        tracing::warn!(
            message_id = %row.id,
            size_bytes = ?row.size_bytes,
            "skipping oversized message body fetch"
        );
        super::recovery::mark_message_fetch_error(db, &row.id, "message too large").await?;
        return Ok(false);
    }

    let uid = match parse_imap_uid(row.external_id.as_deref()) {
        Ok(uid) => uid,
        Err(err) => {
            tracing::warn!(
                message_id = %row.id,
                external_id = ?row.external_id,
                error = %err,
                "cannot lazy-fill body: unparseable IMAP UID"
            );
            super::recovery::mark_message_fetch_error(db, &row.id, "unparseable IMAP UID").await?;
            return Ok(false);
        }
    };

    let (mut client, _) = match connect_imap_for_account(db, user_id, &row.account_id).await {
        Ok(pair) => pair,
        // Account was deleted or deactivated (credential failure) after the
        // message was synced — the reader must still render stored metadata
        // instead of failing the whole request.
        Err(SyncError::AccountNotFound) => {
            tracing::warn!(
                message_id = %row.id,
                account_id = %row.account_id,
                "skipping lazy body fill: account not found or inactive"
            );
            super::recovery::mark_message_fetch_error(db, &row.id, "account not found").await?;
            return Ok(false);
        }
        Err(err) => return Err(err),
    };
    client.select(&row.folder_name).await?;
    let bodies = client.fetch_bodies(&[uid]).await?;
    let Some(fetched) = bodies.into_iter().next() else {
        tracing::warn!(
            message_id = %row.id,
            uid,
            folder = %row.folder_name,
            "IMAP body fetch returned no data"
        );
        super::recovery::mark_message_fetch_error(db, &row.id, "body fetch returned no data")
            .await?;
        return Ok(false);
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
        return Ok(false);
    }

    // DKIM: the raw RFC 822 bytes are in hand exactly once — verify now.
    let dkim_verdict = if let Some(raw) = &fetched.body {
        Some(crate::dkim::verify_raw(raw, &from_domain_of(row)).await)
    } else {
        None
    };

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
    // Negative-cache messages that genuinely have no text or HTML body (e.g.
    // attachment-only): persist an empty text body so every open doesn't
    // re-hit the IMAP server.
    let fetched_body_text = match (&fetched.body_text, &body_html) {
        (None, None) => Some(String::new()),
        _ => fetched.body_text.clone(),
    };

    let mut update = Sq::update();
    update
        .table(message::Entity)
        .value(
            message::Column::BodyText,
            owned_text_value(fetched_body_text.clone()),
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
        );
    // Addresses: heal encoded-word/U+FFFD mojibake when the fetch produced a
    // value; otherwise keep the legacy fill-only-if-absent semantics so a
    // header-less fetch never wipes the stored column.
    if from_json.is_some() {
        update.value(
            message::Column::FromAddress,
            refresh_if_stale(
                message::Column::FromAddress,
                opt_json_value(db, from_json.as_deref()),
            ),
        );
    } else {
        update.value(
            message::Column::FromAddress,
            coalesce_existing(
                message::Column::FromAddress,
                opt_json_value(db, from_json.as_deref()),
            ),
        );
    }
    if to_json.is_some() {
        update.value(
            message::Column::ToAddresses,
            refresh_if_stale(
                message::Column::ToAddresses,
                opt_json_value(db, to_json.as_deref()),
            ),
        );
    } else {
        update.value(
            message::Column::ToAddresses,
            coalesce_existing(
                message::Column::ToAddresses,
                opt_json_value(db, to_json.as_deref()),
            ),
        );
    }
    update
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
    if let Some(v) = &dkim_verdict {
        let headers_json = serde_json::to_string(&v.signed_headers).unwrap_or_default();
        let warnings_json = serde_json::to_string(&v.warnings).unwrap_or_default();
        update
            .value(
                message::Column::DkimStatus,
                opt_str_value(Some(v.status.as_str())),
            )
            .value(message::Column::DkimSdid, opt_str_value(v.sdid.as_deref()))
            .value(message::Column::DkimAuid, opt_str_value(v.auid.as_deref()))
            .value(
                message::Column::DkimSelector,
                opt_str_value(v.selector.as_deref()),
            )
            .value(
                message::Column::DkimAlgorithm,
                opt_str_value(v.algorithm.as_deref()),
            )
            .value(message::Column::DkimSignedHeaders, Expr::val(headers_json))
            .value(message::Column::DkimWarnings, Expr::val(warnings_json))
            .value(message::Column::DkimSignedAt, ts_value(db, v.signed_at))
            .value(message::Column::DkimExpiresAt, ts_value(db, v.expires_at));
    }
    db.orm().execute(&update).await.map_err(orm_err)?;

    row.body_text = fetched_body_text;
    row.body_html = body_html;
    row.has_attachments = fetched.has_attachments || !fetched.attachments.is_empty();
    if header_needs_refresh(row.subject.as_deref()) {
        row.subject.clone_from(&fetched.subject);
    }
    if from_json.is_some() && header_needs_refresh(row.from_address.as_deref()) {
        row.from_address = from_json;
    }
    if to_json.is_some() && header_needs_refresh(row.to_addresses.as_deref()) {
        row.to_addresses = to_json;
    }
    if row.date.is_none() {
        row.date.clone_from(&fetched.date);
    }
    if header_needs_refresh(row.snippet.as_deref()) {
        row.snippet = snippet;
    }
    if let Some(v) = &dkim_verdict {
        row.dkim_status = Some(v.status.as_str().to_string());
        row.dkim_sdid.clone_from(&v.sdid);
        row.dkim_auid.clone_from(&v.auid);
        row.dkim_selector.clone_from(&v.selector);
        row.dkim_algorithm.clone_from(&v.algorithm);
        row.dkim_signed_headers =
            Some(serde_json::to_string(&v.signed_headers).unwrap_or_default());
        row.dkim_warnings = Some(serde_json::to_string(&v.warnings).unwrap_or_default());
        row.dkim_signed_at = v.signed_at.map(|d| d.to_rfc3339());
        row.dkim_expires_at = v.expires_at.map(|d| d.to_rfc3339());
    }
    persist_attachments(db, data_dir, &row.account_id, &row.id, &fetched.attachments).await?;
    Ok(dkim_verdict.is_some())
}

/// PATCH /api/v1/messages/{id} — update read/starred flags (IMAP STORE / JMAP Email/set keywords).
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
    } else if row.protocol == "jmap" && (body.is_read.is_some() || body.is_starred.is_some()) {
        let email_id = row
            .external_id
            .as_deref()
            .ok_or_else(|| SyncError::InvalidInput("JMAP message has no server id".into()))?;
        let seam = connect_jmap_for_account(db, &user_id, &row.account_id).await?;
        if let Err(err) = seam
            .set_email_keywords(email_id, body.is_read, body.is_starred)
            .await
        {
            if err.is_auth() {
                crate::sync::jmap_client::JmapSeam::evict(&row.account_id);
            }
            return Err(err.into());
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
    let db = state.db();
    // Snapshot the sender before the move for spam learning.
    let sender = load_message_row(db, &user_id, &message_id)
        .await
        .ok()
        .and_then(|row| crate::spam::from_json_email(row.from_address.as_deref()));
    let res = move_message_to_role(state.clone(), message_id, user_id.clone(), "spam").await?;
    if let Some(from) = sender {
        crate::spam::learn_sender(db, &user_id, &from, true).await;
    }
    Ok(res)
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
            .and_where(Expr::cust("COALESCE(role_override, role)").eq(Expr::val(role)))
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
    let dest_name = external_id.clone().unwrap_or(name);

    apply_message_move(
        db,
        &user_id,
        &row,
        &dest_id,
        external_id,
        dest_name,
        Some(role),
    )
    .await?;

    Ok(Json(serde_json::json!({
        "status": "ok",
        "action": "moved",
        "role": role,
        "folderId": dest_id,
    })))
}

/// POST /api/v1/messages/{id}/move — file a message into a chosen folder of
/// its own account (cross-account moves are rejected).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MoveMessageRequest {
    folder_id: String,
}

pub(crate) async fn move_message(
    State(state): State<AuthState>,
    Path(message_id): Path<String>,
    AuthUser(user_id): AuthUser,
    Json(body): Json<MoveMessageRequest>,
) -> Result<Json<serde_json::Value>, SyncError> {
    let db = state.db();
    let row = load_message_row(db, &user_id, &message_id).await?;

    let folder_value = id_value(db, &body.folder_id)?;
    let dest_role_value = folder_value.clone();
    let dest = query_first(db, |q| {
        q.expr_as(Expr::col(folder::Column::Id), Alias::new("id"))
            .expr_as(
                Expr::col(folder::Column::AccountId),
                Alias::new("account_id"),
            )
            .expr_as(
                Expr::col(folder::Column::ExternalId),
                Alias::new("external_id"),
            )
            .expr_as(Expr::col(folder::Column::Name), Alias::new("name"))
            .from(folder::Entity)
            .and_where(Expr::col(folder::Column::Id).eq(Expr::val(folder_value)));
    })
    .await?
    .ok_or(SyncError::MessageNotFound)?;

    let dest_id = row_id(&dest, "id").map_err(orm_err)?;
    let dest_account = row_id(&dest, "account_id").map_err(orm_err)?;
    if dest_account != row.account_id {
        return Err(SyncError::InvalidInput(
            "cross-account moves are not supported; pick a folder of the same account".into(),
        ));
    }
    if dest_id == row.folder_id {
        return Ok(Json(serde_json::json!({
            "status": "ok",
            "action": "noop",
            "folderId": dest_id,
        })));
    }

    let external_id: Option<String> = dest.try_get("", "external_id").map_err(orm_err)?;
    let name: String = dest.try_get("", "name").map_err(orm_err)?;
    let dest_name = external_id.clone().unwrap_or(name);
    // Dest role via the table-aliased shape load_message_row uses — an
    // unqualified COALESCE alias decodes as None on the Postgres backend.
    let dest_role: Option<String> = query_first(db, |q| {
        q.expr_as(
            Expr::cust("COALESCE(f.role_override, f.role)"),
            Alias::new("role"),
        )
        .from_as(folder::Entity, Alias::new("f"))
        .and_where(Expr::cust("f.id").eq(Expr::val(dest_role_value)));
    })
    .await?
    .and_then(|row| row.try_get::<Option<String>>("", "role").ok().flatten());

    apply_message_move(
        db,
        &user_id,
        &row,
        &dest_id,
        external_id,
        dest_name,
        dest_role.as_deref(),
    )
    .await?;

    Ok(Json(serde_json::json!({
        "status": "ok",
        "action": "moved",
        "folderId": dest_id,
    })))
}

/// POST /api/v1/messages/{id}/copy — duplicate into another folder of the
/// same account (cross-account copies are rejected). Leaves the original.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CopyMessageRequest {
    folder_id: String,
}

pub(crate) async fn copy_message(
    State(state): State<AuthState>,
    Path(message_id): Path<String>,
    AuthUser(user_id): AuthUser,
    Json(body): Json<CopyMessageRequest>,
) -> Result<Json<serde_json::Value>, SyncError> {
    let db = state.db();
    let row = load_message_row(db, &user_id, &message_id).await?;

    let folder_value = id_value(db, &body.folder_id)?;
    let dest = query_first(db, |q| {
        q.expr_as(Expr::col(folder::Column::Id), Alias::new("id"))
            .expr_as(
                Expr::col(folder::Column::AccountId),
                Alias::new("account_id"),
            )
            .expr_as(
                Expr::col(folder::Column::ExternalId),
                Alias::new("external_id"),
            )
            .expr_as(Expr::col(folder::Column::Name), Alias::new("name"))
            .from(folder::Entity)
            .and_where(Expr::col(folder::Column::Id).eq(Expr::val(folder_value)));
    })
    .await?
    .ok_or(SyncError::MessageNotFound)?;

    let dest_id = row_id(&dest, "id").map_err(orm_err)?;
    let dest_account = row_id(&dest, "account_id").map_err(orm_err)?;
    if dest_account != row.account_id {
        return Err(SyncError::InvalidInput(
            "cross-account copies are not supported; pick a folder of the same account".into(),
        ));
    }
    if dest_id == row.folder_id {
        return Ok(Json(serde_json::json!({
            "status": "ok",
            "action": "noop",
            "folderId": dest_id,
        })));
    }

    let external_id: Option<String> = dest.try_get("", "external_id").map_err(orm_err)?;
    let name: String = dest.try_get("", "name").map_err(orm_err)?;
    let dest_name = external_id.clone().unwrap_or(name);

    apply_message_copy(db, &user_id, &row, external_id, dest_name).await?;

    Ok(Json(serde_json::json!({
        "status": "ok",
        "action": "copied",
        "folderId": dest_id,
    })))
}

/// POST /api/v1/drafts — create or replace a draft in the account's Drafts
/// folder. IMAP: APPEND (+ delete-and-expunge of the replaced draft), then a
/// targeted account sync so the appended copy gains a local row (located via
/// the stamped Message-ID). JMAP: `Email/set` create with `$draft` (+ destroy
/// of the replaced draft) and a direct local row upsert.
/// Active account's address + protocol for draft operations.
async fn draft_account_probe(
    db: &DbPool,
    user_id: &str,
    account_id: &str,
) -> Result<(String, String), SyncError> {
    let account_value = id_value(db, account_id)?;
    let user_value = id_value(db, user_id)?;
    let acct = query_first(db, |q| {
        q.expr_as(
            Expr::col(mail_account::Column::EmailAddress),
            Alias::new("email_address"),
        )
        .expr_as(
            Expr::col(mail_account::Column::Protocol),
            Alias::new("protocol"),
        )
        .from(mail_account::Entity)
        .and_where(Expr::col(mail_account::Column::Id).eq(account_value))
        .and_where(Expr::col(mail_account::Column::UserId).eq(Expr::val(user_value)))
        .and_where(Expr::col(mail_account::Column::IsActive).eq(true));
    })
    .await?
    .ok_or(SyncError::AccountNotFound)?;
    Ok((
        acct.try_get("", "email_address").map_err(orm_err)?,
        acct.try_get("", "protocol").map_err(orm_err)?,
    ))
}

/// The account's drafts-role folder: `(local id, external id, name)`.
async fn drafts_folder(
    db: &DbPool,
    account_id: &str,
) -> Result<(String, Option<String>, String), SyncError> {
    let account_value = id_value(db, account_id)?;
    let dest = query_first(db, |q| {
        q.expr_as(Expr::col(folder::Column::Id), Alias::new("id"))
            .expr_as(
                Expr::col(folder::Column::ExternalId),
                Alias::new("external_id"),
            )
            .expr_as(Expr::col(folder::Column::Name), Alias::new("name"))
            .from(folder::Entity)
            .and_where(Expr::col(folder::Column::AccountId).eq(account_value))
            .and_where(Expr::cust("COALESCE(role_override, role)").eq(Expr::val("drafts")))
            .order_by_expr(Expr::col(folder::Column::SortOrder), Order::Asc)
            .limit(1);
    })
    .await?
    .ok_or_else(|| SyncError::InvalidInput("account has no Drafts folder".into()))?;
    Ok((
        row_id(&dest, "id").map_err(orm_err)?,
        dest.try_get("", "external_id").map_err(orm_err)?,
        dest.try_get("", "name").map_err(orm_err)?,
    ))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SaveDraftRequest {
    account_id: String,
    to: Vec<serde_json::Value>,
    #[serde(default)]
    cc: Option<Vec<serde_json::Value>>,
    #[serde(default)]
    bcc: Option<Vec<serde_json::Value>>,
    subject: String,
    body_text: Option<String>,
    body_html: Option<String>,
    in_reply_to: Option<String>,
    references: Option<String>,
    /// Local row id of the draft this save replaces (edit / re-autosave).
    #[serde(default)]
    existing_draft_id: Option<String>,
    /// Inline images referenced as `cid:` from `body_html` (RFC 2392);
    /// persisted inside the draft's MIME so reopens restore them.
    #[serde(default)]
    inline_attachments: Vec<InlineAttachment>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InlineAttachment {
    filename: String,
    content_type: String,
    content_id: String,
    data_base64: String,
}

pub(crate) async fn save_draft(
    State(state): State<AuthState>,
    AuthUser(user_id): AuthUser,
    Json(body): Json<SaveDraftRequest>,
) -> Result<Json<serde_json::Value>, SyncError> {
    let db = state.db();
    let (email_address, protocol) = draft_account_probe(db, &user_id, &body.account_id).await?;
    let (dest_id, dest_external, dest_name) = drafts_folder(db, &body.account_id).await?;

    // Old draft being replaced (validated: exists, owned, in a drafts folder).
    let old = match &body.existing_draft_id {
        Some(id) => {
            let row = load_message_row(db, &user_id, id).await?;
            if row.folder_role.as_deref() != Some("drafts") {
                return Err(SyncError::InvalidInput("message is not a draft".into()));
            }
            Some(row)
        }
        None => None,
    };

    let outbound = draft_outbound_message(email_address, &body, state.max_attachment_bytes)?;

    match protocol.as_str() {
        "imap" => {
            let raw = crate::smtp::build_message(&outbound)
                .map_err(|e| SyncError::InvalidInput(format!("draft build: {e}")))?
                .formatted();
            let (mut client, _) = connect_imap_for_account(db, &user_id, &body.account_id).await?;
            client
                .append_draft(&dest_external.clone().unwrap_or(dest_name), &raw)
                .await?;
            if let Some(old) = &old
                && let Ok(uid) = parse_imap_uid(old.external_id.as_deref())
            {
                client.select(&old.folder_name).await?;
                client.delete_uid(uid).await?;
            }
            drop(client);

            if let Some(old) = &old {
                soft_delete_message_row(db, &old.id).await?;
                update_folder_counts(db, &old.folder_id).await?;
            }
            // Sync so the appended draft gains a local row; then locate it by
            // the Message-ID we stamped. Offline accounts still return saved.
            let _ = crate::sync::imap_sync_account(db, &user_id, &body.account_id).await;
            let header = outbound.message_id.clone().unwrap_or_default();
            let new_id = message_id_by_header(db, &body.account_id, &header).await?;
            Ok(Json(serde_json::json!({
                "status": "saved",
                "draftMessageId": new_id,
            })))
        }
        "jmap" => {
            let seam = connect_jmap_for_account(db, &user_id, &body.account_id).await?;
            let server_id =
                jmap_replace_draft(&seam, &body.account_id, &outbound, old.as_ref()).await?;
            if let Some(old) = &old {
                soft_delete_message_row(db, &old.id).await?;
                update_folder_counts(db, &old.folder_id).await?;
            }
            let local_id =
                upsert_jmap_draft_row(db, &body.account_id, &dest_id, &server_id, &outbound)
                    .await?;
            persist_draft_attachments(db, &state.data_dir, &body.account_id, &local_id, &outbound)
                .await?;
            Ok(Json(serde_json::json!({
                "status": "saved",
                "draftMessageId": local_id,
            })))
        }
        other => Err(SyncError::InvalidInput(format!(
            "unsupported receive protocol: {other}"
        ))),
    }
}

/// Save-draft request → outbound message, stamping a draft Message-ID so a
/// later sync can locate the appended/imported copy.
fn draft_outbound_message(
    email_address: String,
    body: &SaveDraftRequest,
    max_attachment_bytes: u64,
) -> Result<crate::smtp::OutboundMessage, SyncError> {
    let to = crate::sync::parse_address_list(&body.to);
    if to.is_empty() && body.subject.trim().is_empty() {
        return Err(SyncError::InvalidInput(
            "draft needs recipients or a subject".into(),
        ));
    }
    Ok(crate::smtp::OutboundMessage {
        from_email: email_address,
        from_name: None,
        to,
        cc: body
            .cc
            .as_ref()
            .map(|v| crate::sync::parse_address_list(v))
            .unwrap_or_default(),
        bcc: body
            .bcc
            .as_ref()
            .map(|v| crate::sync::parse_address_list(v))
            .unwrap_or_default(),
        subject: body.subject.clone(),
        body_text: body.body_text.clone(),
        body_html: body.body_html.clone(),
        in_reply_to: body.in_reply_to.clone(),
        references: body.references.clone(),
        mime_content_type: None,
        mime_body: None,
        attachments: parse_inline_attachments(&body.inline_attachments, max_attachment_bytes)?,
        message_id: Some(format!(
            "<lyra-draft-{}@lyra>",
            uuid::Uuid::new_v7(uuid::Timestamp::now(uuid::NoContext))
        )),
    })
}

/// Draft inline images → outbound inline parts; same caps as send.
fn parse_inline_attachments(
    inline: &[InlineAttachment],
    max_bytes: u64,
) -> Result<Vec<crate::smtp::OutboundAttachment>, SyncError> {
    use base64::Engine as _;
    let mut out = Vec::with_capacity(inline.len());
    for att in inline {
        crate::smtp::validate_content_id(&att.content_id)
            .map_err(|e| SyncError::InvalidInput(e.to_string()))?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(att.data_base64.as_bytes())
            .map_err(|e| {
                SyncError::InvalidInput(format!("inline {}: bad base64: {e}", att.filename))
            })?;
        if bytes.len() as u64 > max_bytes {
            return Err(SyncError::InvalidInput(format!(
                "inline image {} exceeds {max_bytes} bytes",
                att.filename
            )));
        }
        out.push(crate::smtp::OutboundAttachment::from_bytes_inline(
            &att.filename,
            &att.content_type,
            &bytes,
            &att.content_id,
        ));
    }
    Ok(out)
}

/// JMAP arm of `save_draft`: create the server draft, then best-effort
/// destroy the one it replaces. Returns the new draft's server id.
async fn jmap_replace_draft(
    seam: &crate::sync::jmap_client::JmapSeam,
    account_id: &str,
    outbound: &crate::smtp::OutboundMessage,
    old: Option<&MessageRow>,
) -> Result<String, SyncError> {
    let server_id = match seam.create_draft(outbound).await {
        Ok(id) => id,
        Err(err) => {
            if err.is_auth() {
                crate::sync::jmap_client::JmapSeam::evict(account_id);
            }
            return Err(err.into());
        }
    };
    if let Some(ext) = old.and_then(|o| o.external_id.as_deref()) {
        // Best-effort: an orphaned server draft is resurrected by the next
        // sync, so log the failure for diagnosability.
        if let Err(err) = seam.destroy_email(ext).await {
            tracing::warn!(
                account_id = %account_id,
                email_id = ext,
                error = %err,
                "failed to destroy replaced JMAP draft"
            );
        }
    }
    Ok(server_id)
}

/// DELETE /api/v1/messages/{id}/draft — discard a draft server-side + local.
pub(crate) async fn discard_draft(
    State(state): State<AuthState>,
    Path(message_id): Path<String>,
    AuthUser(user_id): AuthUser,
) -> Result<Json<serde_json::Value>, SyncError> {
    let db = state.db();
    let row = load_message_row(db, &user_id, &message_id).await?;
    if row.folder_role.as_deref() != Some("drafts") {
        return Err(SyncError::InvalidInput("message is not a draft".into()));
    }

    match row.protocol.as_str() {
        "imap" => {
            let uid = parse_imap_uid(row.external_id.as_deref())?;
            let (mut client, _) = connect_imap_for_account(db, &user_id, &row.account_id).await?;
            client.select(&row.folder_name).await?;
            client.delete_uid(uid).await?;
        }
        "jmap" => {
            let email_id = row
                .external_id
                .as_deref()
                .ok_or_else(|| SyncError::InvalidInput("JMAP draft has no server id".into()))?;
            let seam = connect_jmap_for_account(db, &user_id, &row.account_id).await?;
            if let Err(err) = seam.destroy_email(email_id).await {
                if err.is_auth() {
                    crate::sync::jmap_client::JmapSeam::evict(&row.account_id);
                }
                return Err(err.into());
            }
        }
        other => {
            return Err(SyncError::InvalidInput(format!(
                "unsupported receive protocol: {other}"
            )));
        }
    }

    soft_delete_message_row(db, &row.id).await?;
    update_folder_counts(db, &row.folder_id).await?;
    Ok(Json(serde_json::json!({ "status": "discarded" })))
}

/// Local soft-delete of a message row.
async fn soft_delete_message_row(db: &DbPool, message_id: &str) -> Result<(), SyncError> {
    let mut update = Sq::update();
    update
        .table(message::Entity)
        .value(message::Column::IsDeleted, true)
        .value(message::Column::UpdatedAt, now_value(db))
        .and_where(Expr::col(message::Column::Id).eq(id_value(db, message_id)?));
    db.orm().execute(&update).await.map_err(orm_err)?;
    Ok(())
}

/// Newest local row for an account matching a Message-ID header.
pub(crate) async fn message_id_by_header(
    db: &DbPool,
    account_id: &str,
    header: &str,
) -> Result<Option<String>, SyncError> {
    let account_value = id_value(db, account_id)?;
    let row = query_first(db, |q| {
        q.expr_as(Expr::col(message::Column::Id), Alias::new("id"))
            .from(message::Entity)
            .and_where(Expr::col(message::Column::AccountId).eq(account_value))
            .and_where(Expr::col(message::Column::MessageIdHeader).eq(header))
            .and_where(Expr::col(message::Column::IsDeleted).eq(false))
            .order_by_expr(Expr::col(message::Column::CreatedAt), Order::Desc)
            .limit(1);
    })
    .await?;
    match row {
        Some(r) => Ok(Some(row_id(&r, "id").map_err(orm_err)?)),
        None => Ok(None),
    }
}

/// Insert a local row for a JMAP draft created via `Email/set` (the drafts
/// loop will reconcile it on the next full sync).
async fn upsert_jmap_draft_row(
    db: &DbPool,
    account_id: &str,
    folder_id: &str,
    server_id: &str,
    outbound: &crate::smtp::OutboundMessage,
) -> Result<String, SyncError> {
    // Replace any prior local row bound to this server id.
    let existing = message_id_by_server_id(db, account_id, server_id).await?;
    if let Some(id) = &existing {
        let mut update = Sq::update();
        update
            .table(message::Entity)
            .value(message::Column::Subject, outbound.subject.clone())
            .value(
                message::Column::BodyText,
                outbound.body_text.clone().unwrap_or_default(),
            )
            .value(message::Column::BodyHtml, outbound.body_html.clone())
            .value(message::Column::UpdatedAt, now_value(db))
            .and_where(Expr::col(message::Column::Id).eq(id_value(db, id)?));
        db.orm().execute(&update).await.map_err(orm_err)?;
        return Ok(id.clone());
    }

    let id = uuid::Uuid::new_v7(uuid::Timestamp::now(uuid::NoContext)).to_string();
    let to_json = serde_json::to_string(
        &outbound
            .to
            .iter()
            .map(|(n, e)| match n {
                Some(name) => serde_json::json!({"name": name, "email": e}),
                None => serde_json::json!({"email": e}),
            })
            .collect::<Vec<_>>(),
    )
    .map_err(|e| SyncError::InvalidInput(e.to_string()))?;
    let snippet: String = outbound
        .body_text
        .as_deref()
        .unwrap_or_default()
        .chars()
        .take(120)
        .collect();

    let mut insert = Sq::insert();
    insert
        .into_table(message::Entity)
        .columns([
            message::Column::Id,
            message::Column::AccountId,
            message::Column::FolderId,
            message::Column::ExternalId,
            message::Column::MessageIdHeader,
            message::Column::Subject,
            message::Column::FromAddress,
            message::Column::ToAddresses,
            message::Column::BodyText,
            message::Column::Snippet,
            message::Column::IsDraft,
        ])
        .values_panic(vec![
            id_value(db, &id)?.into(),
            id_value(db, account_id)?.into(),
            id_value(db, folder_id)?.into(),
            Expr::val(server_id.to_owned()),
            Expr::val(outbound.message_id.clone().unwrap_or_default()),
            Expr::val(outbound.subject.clone()),
            // JSON columns are jsonb on PostgreSQL: bind dialect-aware
            // (plain Expr::val(String) is a text expression PG rejects).
            Expr::val(opt_json_value(
                db,
                Some(&serde_json::json!({"email": outbound.from_email}).to_string()),
            )),
            Expr::val(opt_json_value(db, Some(&to_json))),
            Expr::val(outbound.body_text.clone().unwrap_or_default()),
            Expr::val(snippet),
            true.into(),
        ]);
    db.orm().execute(&insert).await.map_err(orm_err)?;
    update_folder_counts(db, folder_id).await?;
    Ok(id)
}

/// Persist a JMAP draft's locally-authored attachments (blob bytes + rows)
/// so reopen-before-sync resolves inline parts and `/attachments/:id/download`
/// serves them — the download endpoint reads local `storage_path`, not the
/// server. Mirrors what the sync parse does for received mail.
async fn persist_draft_attachments(
    db: &DbPool,
    data_dir: &std::path::Path,
    account_id: &str,
    message_id: &str,
    outbound: &crate::smtp::OutboundMessage,
) -> Result<(), SyncError> {
    if outbound.attachments.is_empty() {
        return Ok(());
    }
    let mut extracted = Vec::with_capacity(outbound.attachments.len());
    for att in &outbound.attachments {
        extracted.push(crate::imap::ExtractedAttachment {
            filename: att.filename.clone(),
            content_type: att.content_type.clone(),
            data: att.decode().map_err(|e| {
                SyncError::InvalidInput(format!("attachment {}: {e}", att.filename))
            })?,
            content_id: att.content_id.clone(),
            is_inline: att.content_id.is_some(),
        });
    }
    persist_attachments(db, data_dir, account_id, message_id, &extracted).await
}

pub(crate) async fn message_id_by_server_id(
    db: &DbPool,
    account_id: &str,
    server_id: &str,
) -> Result<Option<String>, SyncError> {
    let account_value = id_value(db, account_id)?;
    let row = query_first(db, |q| {
        q.expr_as(Expr::col(message::Column::Id), Alias::new("id"))
            .from(message::Entity)
            .and_where(Expr::col(message::Column::AccountId).eq(account_value))
            .and_where(Expr::col(message::Column::ExternalId).eq(server_id))
            .and_where(Expr::col(message::Column::IsDeleted).eq(false))
            .limit(1);
    })
    .await?;
    match row {
        Some(r) => Ok(Some(row_id(&r, "id").map_err(orm_err)?)),
        None => Ok(None),
    }
}

/// Server-side move (IMAP UID MOVE / JMAP `mailboxIds` update) plus the local
/// `folder_id` rewrite and folder count refresh. `dest_external` is the
/// protocol-level destination id; IMAP tolerates `None` (name fallback).
pub(crate) async fn apply_message_move(
    db: &DbPool,
    user_id: &str,
    row: &MessageRow,
    dest_id: &str,
    dest_external: Option<String>,
    dest_name: String,
    dest_role: Option<&str>,
) -> Result<(), SyncError> {
    // Learn: moving out of the spam folder is the "not spam" signal.
    if row.folder_role.as_deref() == Some("spam")
        && dest_role.is_some_and(|r| r != "spam")
        && !row.is_draft
        && let Some(from) = crate::spam::from_json_email(row.from_address.as_deref())
    {
        let learned = crate::spam::learn_sender(db, user_id, &from, false).await;
        tracing::info!(learned, from = %from, "spam learn outcome");
    }
    match row.protocol.as_str() {
        "imap" => {
            let uid = parse_imap_uid(row.external_id.as_deref())?;
            let (mut client, _) = connect_imap_for_account(db, user_id, &row.account_id).await?;
            client.select(&row.folder_name).await?;
            client.move_uid(uid, &dest_name).await?;
        }
        "jmap" => {
            let email_id = row
                .external_id
                .as_deref()
                .ok_or_else(|| SyncError::InvalidInput("JMAP message has no server id".into()))?;
            let mailbox_id = dest_external.clone().ok_or_else(|| {
                SyncError::InvalidInput("JMAP target folder has no server id".into())
            })?;
            let seam = connect_jmap_for_account(db, user_id, &row.account_id).await?;
            if let Err(err) = seam.set_email_mailboxes(email_id, &[mailbox_id]).await {
                if err.is_auth() {
                    crate::sync::jmap_client::JmapSeam::evict(&row.account_id);
                }
                return Err(err.into());
            }
        }
        other => {
            return Err(SyncError::InvalidInput(format!(
                "unsupported receive protocol: {other}"
            )));
        }
    }

    let mut move_update = Sq::update();
    move_update
        .table(message::Entity)
        .value(message::Column::FolderId, id_value(db, dest_id)?)
        .value(message::Column::UpdatedAt, now_value(db))
        .and_where(Expr::col(message::Column::Id).eq(id_value(db, &row.id)?));
    db.orm().execute(&move_update).await.map_err(orm_err)?;

    update_folder_counts(db, &row.folder_id).await?;
    update_folder_counts(db, dest_id).await?;
    Ok(())
}

/// Server-side copy (IMAP UID COPY / JMAP mailboxIds union). Does not rewrite
/// the local `folder_id` — the duplicate appears after the next sync.
pub(crate) async fn apply_message_copy(
    db: &DbPool,
    user_id: &str,
    row: &MessageRow,
    dest_external: Option<String>,
    dest_name: String,
) -> Result<(), SyncError> {
    match row.protocol.as_str() {
        "imap" => {
            let uid = parse_imap_uid(row.external_id.as_deref())?;
            let (mut client, _) = connect_imap_for_account(db, user_id, &row.account_id).await?;
            client.select(&row.folder_name).await?;
            client.copy_uid(uid, &dest_name).await?;
        }
        "jmap" => {
            let email_id = row
                .external_id
                .as_deref()
                .ok_or_else(|| SyncError::InvalidInput("JMAP message has no server id".into()))?;
            let mailbox_id = dest_external.ok_or_else(|| {
                SyncError::InvalidInput("JMAP target folder has no server id".into())
            })?;
            let seam = connect_jmap_for_account(db, user_id, &row.account_id).await?;
            if let Err(err) = seam.add_email_mailbox(email_id, &mailbox_id).await {
                if err.is_auth() {
                    crate::sync::jmap_client::JmapSeam::evict(&row.account_id);
                }
                return Err(err.into());
            }
        }
        other => {
            return Err(SyncError::InvalidInput(format!(
                "unsupported receive protocol: {other}"
            )));
        }
    }
    Ok(())
}

// ── Anti-spam pass ───────────────────────────────────────────────────

/// Post-sync anti-spam pass for one user: judge not-yet-judged inbox
/// messages under their settings/lists, move flagged ones to the account's
/// spam folder, and (when auto-delete is on) destroy spam-folder messages
/// older than 30 days. Cheap no-op unless the feature is configured.
pub(crate) async fn spam_pass(db: &DbPool, user_id: &str) {
    if let Err(err) = spam_pass_inner(db, user_id).await {
        tracing::warn!(user_id, error = %err, "spam pass failed");
    }
}

const SPAM_PASS_BATCH: u64 = 100;
const SPAM_PURGE_DAYS: i64 = 30;

fn spam_db_err(e: crate::spam::SpamStoreError) -> sqlx::Error {
    match e {
        crate::spam::SpamStoreError::Database(db) => db,
        crate::spam::SpamStoreError::InvalidId(m) => sqlx::Error::Protocol(m),
    }
}

async fn spam_pass_inner(db: &DbPool, user_id: &str) -> Result<(), SyncError> {
    let settings = crate::spam::load_settings(db, user_id)
        .await
        .map_err(|e| SyncError::Database(spam_db_err(e)))?;
    if !settings.enabled && !settings.auto_delete {
        return Ok(());
    }
    let senders = crate::spam::list_senders(db, user_id)
        .await
        .map_err(|e| SyncError::Database(spam_db_err(e)))?;

    if settings.enabled {
        judge_inbox_batch(db, user_id, &settings, &senders).await?;
    }
    if settings.auto_delete {
        purge_old_spam(db, user_id).await?;
    }
    Ok(())
}

/// Judge one batch of unjudged inbox messages; spam/blocked verdicts move
/// to the spam folder. Move failures are logged and skipped — the verdict
/// stays recorded so one bad server response cannot re-judge forever.
async fn judge_inbox_batch(
    db: &DbPool,
    user_id: &str,
    settings: &crate::spam::SpamSettings,
    senders: &[crate::spam::SenderEntry],
) -> Result<(), SyncError> {
    let rows = unjudged_rows(db, user_id, "inbox", SPAM_PASS_BATCH).await?;
    for row in rows {
        let env = crate::spam::SpamEnvelope {
            from_email: row.from_email.as_deref(),
            from_name: None,
            subject: row.subject.as_deref(),
        };
        let Some(verdict) = crate::spam::judge_message(&env, settings, senders) else {
            continue;
        };
        set_spam_verdict(db, &row.id, &verdict).await?;
        if verdict == "spam" || verdict == "blocked" {
            match move_row_to_spam(db, user_id, &row.id).await {
                Ok(()) => {}
                Err(err) => {
                    tracing::warn!(
                        message_id = %row.id,
                        error = %err,
                        "spam move failed; verdict recorded"
                    );
                }
            }
        }
    }
    Ok(())
}

struct SpamCandidate {
    id: String,
    from_email: Option<String>,
    subject: Option<String>,
    date: Option<String>,
}

/// Read the `date` column dialect-aware (TEXT on SQLite, timestamp on PG).
fn row_date_text(row: &sea_orm::QueryResult) -> Option<String> {
    row.try_get::<Option<String>>("", "date")
        .ok()
        .flatten()
        .or_else(|| {
            row.try_get::<Option<chrono::DateTime<chrono::Utc>>>("", "date")
                .ok()
                .flatten()
                .map(|d| d.to_rfc3339())
        })
}

/// Messages in folders with `role` that have no spam verdict yet.
async fn unjudged_rows(
    db: &DbPool,
    user_id: &str,
    role: &str,
    limit: u64,
) -> Result<Vec<SpamCandidate>, SyncError> {
    let user_value = id_value(db, user_id)?;
    let mut q = Sq::select();
    q.expr_as(aliased_col("m", "id"), Alias::new("id"))
        .expr_as(aliased_col("m", "from_address"), Alias::new("from_address"))
        .expr_as(aliased_col("m", "subject"), Alias::new("subject"))
        .expr_as(aliased_col("m", "date"), Alias::new("date"));
    add_message_account_join(&mut q);
    add_message_folder_join(&mut q);
    q.and_where(aliased_col("a", "user_id").eq(Expr::val(user_value)))
        .and_where(aliased_col("m", "is_deleted").eq(Expr::val(false)))
        .and_where(aliased_col("m", "spam_verdict").is_null())
        .and_where(Expr::cust("COALESCE(f.role_override, f.role)").eq(Expr::val(role)))
        .order_by_expr(Expr::cust("m.date"), Order::Desc)
        .limit(limit);
    let rows = db.orm().query_all(&q).await.map_err(orm_err)?;
    Ok(rows
        .iter()
        .map(|row| SpamCandidate {
            id: row_id(row, "id").map_err(orm_err).unwrap_or_default(),
            from_email: row_json_text(row, "from_address")
                .ok()
                .and_then(|raw| crate::spam::from_json_email(raw.as_deref())),
            subject: row.try_get("", "subject").ok().flatten(),
            date: row_date_text(row),
        })
        .collect())
}

async fn set_spam_verdict(db: &DbPool, message_id: &str, verdict: &str) -> Result<(), SyncError> {
    let mut upd = Sq::update();
    upd.table(message::Entity)
        .value(message::Column::SpamVerdict, Expr::val(verdict))
        .value(message::Column::UpdatedAt, now_value(db))
        .and_where(Expr::col(message::Column::Id).eq(id_value(db, message_id)?));
    db.orm().execute(&upd).await.map_err(orm_err)?;
    Ok(())
}

/// Full-row move of one message to its account's spam folder.
async fn move_row_to_spam(db: &DbPool, user_id: &str, message_id: &str) -> Result<(), SyncError> {
    let row = load_message_row(db, user_id, message_id).await?;
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
            .and_where(Expr::cust("COALESCE(role_override, role)").eq(Expr::val("spam")))
            .order_by_expr(Expr::col(folder::Column::SortOrder), Order::Asc)
            .limit(1);
    })
    .await?
    .ok_or_else(|| SyncError::InvalidInput("account has no spam folder".into()))?;
    let dest_id = row_id(&dest, "id").map_err(orm_err)?;
    let external: Option<String> = dest.try_get("", "external_id").map_err(orm_err)?;
    let name: String = dest.try_get("", "name").map_err(orm_err)?;
    apply_message_move(
        db,
        user_id,
        &row,
        &dest_id,
        external.clone(),
        external.unwrap_or(name),
        Some("spam"),
    )
    .await
}

/// Destroy spam-folder messages older than the purge window, server-side
/// first, then locally.
async fn purge_old_spam(db: &DbPool, user_id: &str) -> Result<(), SyncError> {
    let cutoff = chrono::Utc::now() - chrono::Duration::days(SPAM_PURGE_DAYS);
    let rows = spam_folder_rows(db, user_id).await?;
    for row in rows {
        let old = row
            .date
            .as_deref()
            .and_then(|d| chrono::DateTime::parse_from_rfc3339(d).ok())
            .is_some_and(|d| d.with_timezone(&chrono::Utc) < cutoff);
        if !old {
            continue;
        }
        match destroy_row_server_side(db, user_id, &row.id).await {
            Ok(()) => {}
            Err(err) => {
                tracing::warn!(message_id = %row.id, error = %err, "spam purge destroy failed");
            }
        }
    }
    Ok(())
}

/// Spam-folder rows (any verdict) for the purge scan.
async fn spam_folder_rows(db: &DbPool, user_id: &str) -> Result<Vec<SpamCandidate>, SyncError> {
    let user_value = id_value(db, user_id)?;
    let mut q = Sq::select();
    q.expr_as(aliased_col("m", "id"), Alias::new("id"));
    add_message_account_join(&mut q);
    add_message_folder_join(&mut q);
    q.and_where(aliased_col("a", "user_id").eq(Expr::val(user_value)))
        .and_where(aliased_col("m", "is_deleted").eq(Expr::val(false)))
        .and_where(Expr::cust("COALESCE(f.role_override, f.role)").eq(Expr::val("spam")))
        .order_by_expr(Expr::cust("m.date"), Order::Asc)
        .limit(200);
    let rows = db.orm().query_all(&q).await.map_err(orm_err)?;
    Ok(rows
        .iter()
        .map(|row| SpamCandidate {
            id: row_id(row, "id").map_err(orm_err).unwrap_or_default(),
            from_email: None,
            subject: None,
            date: row_date_text(row),
        })
        .collect())
}

/// Server-side destroy (IMAP delete/expunge, JMAP Email/destroy) plus the
/// local soft-delete. Mirrors `discard_draft`'s per-protocol structure.
async fn destroy_row_server_side(
    db: &DbPool,
    user_id: &str,
    message_id: &str,
) -> Result<(), SyncError> {
    let row = load_message_row(db, user_id, message_id).await?;
    match row.protocol.as_str() {
        "imap" => {
            let uid = parse_imap_uid(row.external_id.as_deref())?;
            let (mut client, _) = connect_imap_for_account(db, user_id, &row.account_id).await?;
            client.select(&row.folder_name).await?;
            client.delete_uid(uid).await?;
        }
        "jmap" => {
            let email_id = row
                .external_id
                .as_deref()
                .ok_or_else(|| SyncError::InvalidInput("JMAP message has no server id".into()))?;
            let seam = connect_jmap_for_account(db, user_id, &row.account_id).await?;
            if let Err(err) = seam.destroy_email(email_id).await {
                if err.is_auth() {
                    crate::sync::jmap_client::JmapSeam::evict(&row.account_id);
                }
                return Err(err.into());
            }
        }
        other => {
            return Err(SyncError::InvalidInput(format!(
                "unsupported receive protocol: {other}"
            )));
        }
    }
    let mut upd = Sq::update();
    upd.table(message::Entity)
        .value(message::Column::IsDeleted, true)
        .value(message::Column::UpdatedAt, now_value(db))
        .and_where(Expr::col(message::Column::Id).eq(id_value(db, &row.id)?));
    db.orm().execute(&upd).await.map_err(orm_err)?;
    update_folder_counts(db, &row.folder_id).await?;
    Ok(())
}

#[cfg(test)]
#[cfg(feature = "postgres")]
mod postgres_live {
    //! Draft-row roundtrips: `upsert_jmap_draft_row` binds JSON columns —
    //! a plain `Expr::val(String)` is a text expression that PostgreSQL
    //! rejects against jsonb columns.

    use crate::pgtest::support;
    use crate::sync::store;

    #[test]
    #[ignore = "needs postgres"]
    fn unjudged_rows_project_envelope_columns() {
        support::rt().block_on(async {
            let (db, user_id) = support::setup().await;
            let account_id = support::seed_account(&db, &user_id, "judge@example.com").await;
            let folder_id = support::seed_inbox(&db, &account_id).await;
            let mut msg = support::message(41, "Judge me", "judge@example.com");
            msg.date = Some("2026-09-01T00:00:00Z".into());
            store::upsert_message(&db, &account_id, &folder_id, &msg)
                .await
                .unwrap();

            let rows = super::unjudged_rows(&db, &user_id, "inbox", 10)
                .await
                .unwrap();
            let hit = rows
                .iter()
                .find(|r| r.subject.as_deref() == Some("Judge me"))
                .expect("unjudged inbox row is visible");
            assert_eq!(
                hit.from_email.as_deref(),
                Some("judge@example.com"),
                "envelope columns must be projected, not just the id"
            );
            assert!(hit.date.is_some());
        });
    }

    fn draft_outbound() -> crate::smtp::OutboundMessage {
        crate::smtp::OutboundMessage {
            from_email: "sender@example.com".into(),
            from_name: None,
            to: vec![(Some("Rcpt".into()), "rcpt@example.com".into())],
            cc: vec![],
            bcc: vec![],
            subject: "[pg live] draft".into(),
            body_text: Some("draft body".into()),
            body_html: None,
            message_id: Some("<draft-pglive@example.com>".into()),
            in_reply_to: None,
            references: None,
            attachments: vec![],
            mime_content_type: None,
            mime_body: None,
        }
    }

    #[test]
    #[ignore = "needs postgres"]
    fn jmap_draft_row_upsert_roundtrip() {
        support::rt().block_on(async {
            let (db, user_id) = support::setup().await;
            let account_id = support::seed_jmap_account(&db, &user_id, "drafts@example.com").await;
            let folder_id = support::seed_inbox(&db, &account_id).await;

            let first = super::upsert_jmap_draft_row(
                &db,
                &account_id,
                &folder_id,
                "server-draft-1",
                &draft_outbound(),
            )
            .await
            .expect("draft row insert survives jsonb typing");
            let second = super::upsert_jmap_draft_row(
                &db,
                &account_id,
                &folder_id,
                "server-draft-1",
                &draft_outbound(),
            )
            .await
            .expect("second upsert updates, not duplicates");
            assert_eq!(first, second, "same server id maps to the same local row");

            let crate::storage::DbPool::Postgres(pool) = &db else {
                panic!()
            };
            let (count, subject): (i64, String) = sqlx::query_as(
                "SELECT COUNT(*), MIN(subject) FROM message \
                 WHERE account_id = $1::uuid AND external_id = 'server-draft-1'",
            )
            .bind(&account_id)
            .fetch_one(pool)
            .await
            .unwrap();
            assert_eq!(count, 1);
            assert_eq!(subject, "[pg live] draft");
            let _ = store::upsert_message; // module touch (keeps imports honest)
        });
    }
}

#[cfg(test)]
mod lazy_fill_sql_tests {
    use super::*;

    /// Regression: the lazy body-fill UPDATE compared JSONB address columns
    /// to bare text (`col = ''`, `col LIKE …`), which Postgres rejects with
    /// `operator does not exist: jsonb = text` — killing every IMAP body
    /// fetch in production while SQLite tests stayed green. The stale check
    /// must run against `CAST(col AS text)` on both backends.
    #[test]
    fn stale_refresh_casts_jsonb_columns_to_text() {
        let mut update = Sq::update();
        update.table(message::Entity).value(
            message::Column::FromAddress,
            refresh_if_stale(
                message::Column::FromAddress,
                owned_text_value(Some("{\"raw\":\"a@b\"}".into())),
            ),
        );
        let (pg_sql, _params) = update.build(sea_orm::sea_query::PostgresQueryBuilder);
        assert!(
            pg_sql.contains("CAST(\"from_address\" AS text)"),
            "{pg_sql}"
        );
        // Every comparison against the column goes through CAST; the only
        // bare `"from_address" =` left is the SET assignment, whose RHS is
        // the CASE expression, not a text parameter.
        assert!(
            !pg_sql.contains("\"from_address\" = $"),
            "jsonb column compared to bare text: {pg_sql}"
        );
        // SQLite accepts either form; the cast must not corrupt its SQL.
        let (sqlite_sql, _params) = update.build(sea_orm::sea_query::SqliteQueryBuilder);
        assert!(sqlite_sql.contains("CAST("), "{sqlite_sql}");
    }
}

#[cfg(test)]
mod learn_hook_tests {
    use super::*;
    use crate::sync::queries::MessageRow;

    fn spam_row() -> MessageRow {
        MessageRow {
            id: "00000000-0000-7000-8000-0000000000aa".into(),
            in_reply_to: None,
            references_headers: None,
            account_id: "acc".into(),
            folder_id: "fld".into(),
            folder_name: "Spam".into(),
            external_id: None,
            message_id_header: None,
            protocol: "imap".into(),
            folder_role: Some("spam".into()),
            body_text: None,
            body_html: None,
            is_draft: false,
            is_read: true,
            is_starred: false,
            has_attachments: false,
            from_address: Some(r#"{"raw": "cheng@thundermail.com"}"#.into()),
            subject: Some("Learn".into()),
            to_addresses: None,
            cc_addresses: None,
            date: None,
            snippet: None,
            size_bytes: None,
            dkim_status: None,
            dkim_sdid: None,
            dkim_auid: None,
            dkim_selector: None,
            dkim_algorithm: None,
            dkim_signed_headers: None,
            dkim_warnings: None,
            dkim_signed_at: None,
            dkim_expires_at: None,
        }
    }

    /// The learn-allow hook runs *before* the (here failing) protocol move,
    /// so the allowed sender must be recorded even though the move errors.
    #[tokio::test]
    async fn moving_out_of_spam_learns_allowed_sender() {
        let storage = crate::storage::Storage::new("sqlite::memory:")
            .await
            .unwrap();
        storage.run_migrations().await.unwrap();
        let db = storage.pool().clone();
        let DbPool::Sqlite(pool) = &db else { panic!() };
        sqlx::query(
            "INSERT INTO lyra_user (id, username, password_hash, encrypted_dek) \
             VALUES ('u1', 't', 'h', '[]')",
        )
        .execute(pool)
        .await
        .unwrap();
        crate::spam::save_settings(
            &db,
            "u1",
            &crate::spam::SpamSettings {
                enabled: true,
                learn: true,
                auto_delete: false,
                sensitivity: crate::spam::Sensitivity::Standard,
            },
        )
        .await
        .unwrap();
        crate::spam::add_sender(
            &db,
            "u1",
            "cheng@thundermail.com",
            crate::spam::SenderList::Blocked,
        )
        .await
        .unwrap();

        // Protocol move fails (no server); the learn hook must still fire.
        let _ = apply_message_move(
            &db,
            "u1",
            &spam_row(),
            "dest-folder",
            None,
            "INBOX".into(),
            Some("inbox"),
        )
        .await;

        let senders = crate::spam::list_senders(&db, "u1").await.unwrap();
        assert!(
            senders
                .iter()
                .any(|s| s.list == crate::spam::SenderList::Allowed),
            "moving out of spam must learn the allowed sender, got {senders:?}"
        );
    }
}

#[cfg(test)]
mod dkim_lazy_tests {
    use super::*;
    use crate::auth::AuthSession;
    use crate::imap::ImapMessage;
    use crate::kernel::App;
    use crate::storage::Storage;
    use crate::sync::store::{get_folder_id, new_uuid_text, upsert_folder, upsert_message};

    /// In-memory SQLite with a user, an IMAP account whose credential cannot
    /// be decrypted (no reachable server, no network), and one message whose
    /// body is already filled while `dkim_status` stays NULL.
    /// Returns `(state, user_id, message_id)`.
    async fn seed_filled_message() -> (AuthState, String, String) {
        let storage = Storage::new("sqlite::memory:").await.unwrap();
        storage.run_migrations().await.unwrap();
        let db = storage.pool().clone();
        let DbPool::Sqlite(pool) = &db else {
            panic!("sqlite")
        };

        let user_id = new_uuid_text();
        let account_id = new_uuid_text();
        sqlx::query(
            "INSERT INTO lyra_user (id, username, password_hash, encrypted_dek) \
             VALUES (?, ?, 'hash', '[]')",
        )
        .bind(&user_id)
        .bind(format!("dkim-lazy-{user_id}"))
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO mail_account (\
                 id, user_id, display_name, email_address, protocol, auth_type, \
                 credential, imap_host, imap_port, imap_security, is_active, sync_enabled\
             ) VALUES (?, ?, 'DKIM', 'dkim@example.com', 'imap', 'password', \
                       'cred', 'imap.example.com', 993, 'tls', 1, 1)",
        )
        .bind(&account_id)
        .bind(&user_id)
        .execute(pool)
        .await
        .unwrap();
        upsert_folder(&db, &account_id, "INBOX", None, &[])
            .await
            .unwrap();
        let folder_id = get_folder_id(&db, &account_id, "INBOX").await.unwrap();
        let msg = ImapMessage {
            uid: 1,
            message_id: Some("<1@example.com>".into()),
            subject: Some("Verify me".into()),
            from: Some("ops@example.com".into()),
            to: Some("me@example.org".into()),
            cc: None,
            date: None,
            in_reply_to: None,
            references: None,
            flags: vec!["\\Seen".into()],
            size: Some(1024),
            body: None,
            body_text: None,
            body_html: None,
            has_attachments: false,
            attachments: vec![],
        };
        upsert_message(&db, &account_id, &folder_id, &msg)
            .await
            .unwrap();
        let message_id: String = sqlx::query_scalar("SELECT id FROM message WHERE account_id = ?")
            .bind(&account_id)
            .fetch_one(pool)
            .await
            .unwrap();
        // Body already filled → the fill hook is a no-op this request.
        sqlx::query("UPDATE message SET body_text = 'cached body' WHERE id = ?")
            .bind(&message_id)
            .execute(pool)
            .await
            .unwrap();

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
        let state = AuthState::new(
            db,
            &config,
            std::sync::Arc::new(App::new()),
            std::sync::Arc::new(crate::kv::MemoryKv::new()),
        )
        .unwrap();
        (state, user_id, message_id)
    }

    async fn open_message(state: AuthState, user_id: &str, message_id: String) -> MessageResponse {
        let Json(resp) = get_message(
            State(state),
            Path(message_id),
            AuthSession {
                user_id: user_id.to_string(),
                token: "tok".into(),
            },
            Query(GetMessageQuery {
                remote_content: None,
            }),
        )
        .await
        .unwrap();
        resp
    }

    /// The lazy path attempts a refetch; with no reachable server in tests,
    /// the failure-safe contract is: the response serves normally and carries
    /// no dkim object (row stays NULL for a later retry).
    #[tokio::test]
    async fn get_message_without_verdict_serves_dkim_none() {
        let (state, user_id, message_id) = seed_filled_message().await;
        let resp = open_message(state, &user_id, message_id).await;
        assert!(resp.dkim.is_none());
    }

    /// A stored verdict surfaces through the handler and is not refetched.
    #[tokio::test]
    async fn get_message_with_stored_verdict_serves_dkim() {
        let (state, user_id, message_id) = seed_filled_message().await;
        let verdict = crate::dkim::DkimVerdict {
            status: crate::dkim::DkimStatus::Pass,
            sdid: Some("example.com".into()),
            auid: Some("ops@example.com".into()),
            selector: Some("sel1".into()),
            algorithm: Some("RsaSha256".into()),
            signed_headers: vec!["from".into(), "to".into()],
            warnings: Vec::new(),
            signed_at: chrono::DateTime::from_timestamp(1_756_700_000, 0),
            expires_at: None,
        };
        crate::sync::store::update_dkim_verdict(state.db(), &message_id, &verdict)
            .await
            .unwrap();

        let resp = open_message(state, &user_id, message_id).await;
        let dkim = resp.dkim.expect("stored verdict must surface");
        assert_eq!(dkim.status, "pass");
        assert_eq!(dkim.sdid.as_deref(), Some("example.com"));
    }
}

#[cfg(test)]
mod inline_attachment_tests {
    use super::*;

    fn inline(filename: &str, cid: &str, data_b64: &str) -> InlineAttachment {
        InlineAttachment {
            filename: filename.into(),
            content_type: "image/png".into(),
            content_id: cid.into(),
            data_base64: data_b64.into(),
        }
    }

    #[test]
    fn parses_valid_inline_image() {
        let out = parse_inline_attachments(&[inline("a.png", "img1@lyra", "AQID")], 1024).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].filename, "a.png");
        assert_eq!(out[0].content_type, "image/png");
        assert_eq!(out[0].content_id.as_deref(), Some("img1@lyra"));
        assert_eq!(out[0].decode().unwrap(), vec![1, 2, 3]);
    }

    #[test]
    fn rejects_bad_content_id() {
        let err = parse_inline_attachments(&[inline("a.png", "bad>id", "AQID")], 1024).unwrap_err();
        assert!(matches!(err, SyncError::InvalidInput(_)));
    }

    #[test]
    fn rejects_bad_base64() {
        let err =
            parse_inline_attachments(&[inline("a.png", "img1@lyra", "!!!")], 1024).unwrap_err();
        assert!(matches!(err, SyncError::InvalidInput(_)));
    }

    #[test]
    fn rejects_oversize() {
        let err = parse_inline_attachments(&[inline("a.png", "img1@lyra", "AQID")], 2).unwrap_err();
        assert!(matches!(err, SyncError::InvalidInput(_)));
    }
}

#[cfg(test)]
mod draft_attachment_tests {
    //! The seeded SQLite accounts are IMAP, so the JMAP save-draft arm's row
    //! writing is exercised directly: `upsert_jmap_draft_row` +
    //! `persist_draft_attachments`, the same composition `save_draft` runs.
    use super::*;
    use crate::storage::{DbPool, Storage};
    use crate::sync::store::{get_folder_id, new_uuid_text, upsert_folder};

    async fn seed_jmap_account() -> (DbPool, sqlx::SqlitePool, String, String) {
        let storage = Storage::new("sqlite::memory:").await.unwrap();
        storage.run_migrations().await.unwrap();
        let db = storage.pool().clone();
        let DbPool::Sqlite(pool) = &db else {
            panic!("sqlite")
        };
        let user_id = new_uuid_text();
        let account_id = new_uuid_text();
        sqlx::query(
            "INSERT INTO lyra_user (id, username, password_hash, encrypted_dek) \
             VALUES (?, ?, 'hash', '[]')",
        )
        .bind(&user_id)
        .bind(format!("draft-att-{user_id}"))
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO mail_account (\
                 id, user_id, display_name, email_address, protocol, auth_type, \
                 credential, imap_host, imap_port, imap_security, is_active, sync_enabled\
             ) VALUES (?, ?, 'Drafts', 'drafts@example.com', 'jmap', 'password', \
                       'cred', 'jmap.example.com', 443, 'tls', 1, 1)",
        )
        .bind(&account_id)
        .bind(&user_id)
        .execute(pool)
        .await
        .unwrap();
        upsert_folder(&db, &account_id, "Drafts", None, &[])
            .await
            .unwrap();
        let folder_id = get_folder_id(&db, &account_id, "Drafts").await.unwrap();
        let pool = pool.clone();
        (db, pool, account_id, folder_id)
    }

    fn draft_outbound() -> crate::smtp::OutboundMessage {
        crate::smtp::OutboundMessage {
            from_email: "drafts@example.com".into(),
            from_name: None,
            to: vec![(None, "rcpt@example.com".into())],
            cc: vec![],
            bcc: vec![],
            subject: "draft".into(),
            body_text: Some("body".into()),
            body_html: None,
            in_reply_to: None,
            references: None,
            mime_content_type: None,
            mime_body: None,
            attachments: vec![],
            message_id: Some("<draft-att@example.com>".into()),
        }
    }

    #[tokio::test]
    async fn jmap_draft_save_persists_inline_attachment_rows() {
        let (db, pool, account_id, folder_id) = seed_jmap_account().await;
        let mut outbound = draft_outbound();
        outbound
            .attachments
            .push(crate::smtp::OutboundAttachment::from_bytes_inline(
                "a.png",
                "image/png",
                b"\x89PNG",
                "img1@lyra",
            ));

        let local_id = upsert_jmap_draft_row(&db, &account_id, &folder_id, "server-1", &outbound)
            .await
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        persist_draft_attachments(&db, dir.path(), &account_id, &local_id, &outbound)
            .await
            .unwrap();

        let rows: Vec<(String, Option<String>, bool, i64, String)> = sqlx::query_as(
            "SELECT filename, content_id, is_inline, size_bytes, storage_path \
             FROM attachment WHERE message_id = ?",
        )
        .bind(&local_id)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(rows.len(), 1, "inline part must produce an attachment row");
        let (filename, content_id, is_inline, size, storage_path) = &rows[0];
        assert_eq!(filename, "a.png");
        assert_eq!(content_id.as_deref(), Some("img1@lyra"));
        assert!(is_inline);
        assert_eq!(*size, 4);
        // The download endpoint reads local storage_path — bytes must be there.
        let bytes = crate::blobs::read(dir.path(), storage_path).await.unwrap();
        assert_eq!(bytes, b"\x89PNG");

        let has_attachments: bool =
            sqlx::query_scalar("SELECT has_attachments FROM message WHERE id = ?")
                .bind(&local_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(has_attachments);
    }

    #[tokio::test]
    async fn jmap_draft_save_without_attachments_writes_no_rows() {
        let (db, pool, account_id, folder_id) = seed_jmap_account().await;
        let outbound = draft_outbound();
        let local_id = upsert_jmap_draft_row(&db, &account_id, &folder_id, "server-2", &outbound)
            .await
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        persist_draft_attachments(&db, dir.path(), &account_id, &local_id, &outbound)
            .await
            .unwrap();

        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM attachment WHERE message_id = ?")
            .bind(&local_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 0);
    }
}
