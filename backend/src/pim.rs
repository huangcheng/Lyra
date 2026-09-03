//! PIM (Personal Information Management) module.
//!
//! Provides CardDAV contacts and CalDAV calendar sync/view.
//! Uses generic CalDAV/CardDAV protocols — no Google/Outlook-specific APIs.
//!
//! See `docs/specs/2026-08-20-lyra-sync-and-protocols-spec.md`.

#![allow(clippy::doc_markdown)]

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use chrono::{DateTime, Utc};
use sea_orm::sea_query::{Alias, Condition, Expr, ExprTrait, Order, Query as Sq, SelectStatement};
use sea_orm::{ColumnTrait, ConnectionTrait, QueryResult, Value};
use serde::{Deserialize, Serialize};

use crate::api_error::ApiErrorBody;
use crate::auth::{AuthState, AuthUser};
use crate::db_row::{IdParam, InvalidIdError, TsParam, id_param, opt_ts_param};
use crate::entities::{calendar, calendar_event, contact, mail_account};
use crate::storage::DbPool;

/// Routes for PIM endpoints.
pub fn routes() -> Router<AuthState> {
    Router::new()
        // Contacts
        .route("/api/v1/contacts", get(list_contacts))
        .route("/api/v1/contacts/{id}", get(get_contact))
        .route(
            "/api/v1/accounts/{account_id}/contacts/sync",
            get(sync_contacts),
        )
        .route(
            "/api/v1/accounts/{account_id}/pim/discover",
            post(pim_discover),
        )
        // Calendars
        .route("/api/v1/calendars", get(list_calendars))
        .route("/api/v1/calendars/{id}", get(get_calendar))
        .route("/api/v1/calendars/{id}/events", get(list_events))
        .route("/api/v1/events/{id}", get(get_event))
        .route(
            "/api/v1/accounts/{account_id}/calendars/sync",
            get(sync_calendars),
        )
}

/// A contact as returned by the API.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Contact {
    pub id: String,
    pub account_id: String,
    pub display_name: Option<String>,
    pub email_addresses: Vec<String>,
    pub phone_numbers: Vec<String>,
    pub organisation: Option<String>,
    pub photo_path: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// A calendar as returned by the API.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Calendar {
    pub id: String,
    pub account_id: String,
    pub name: String,
    pub color: Option<String>,
    pub description: Option<String>,
    pub timezone: Option<String>,
    pub is_active: bool,
    pub created_at: String,
    pub updated_at: String,
}

/// A calendar event as returned by the API.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CalendarEvent {
    pub id: String,
    pub calendar_id: Option<String>,
    pub summary: Option<String>,
    pub description: Option<String>,
    pub dtstart: Option<String>,
    pub dtend: Option<String>,
    pub location: Option<String>,
    pub is_all_day: bool,
    pub status: Option<String>,
    pub recurrence_rule: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// Query parameters for listing contacts.
#[derive(Debug, Deserialize)]
pub struct ListContactsQuery {
    pub account_id: Option<String>,
    pub q: Option<String>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

/// Query parameters for listing calendars.
#[derive(Debug, Deserialize)]
pub struct ListCalendarsQuery {
    pub account_id: Option<String>,
}

/// Query parameters for listing events.
#[derive(Debug, Deserialize)]
pub struct ListEventsQuery {
    pub start: Option<String>,
    pub end: Option<String>,
}

/// Error type for PIM operations.
#[derive(Debug, thiserror::Error)]
#[allow(dead_code)]
pub enum PimError {
    #[error("not found")]
    NotFound,
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("account not found or not accessible")]
    AccountNotFound,
    #[error("sync error: {0}")]
    SyncError(String),
    #[error("authentication required")]
    Unauthorized,
}

impl From<InvalidIdError> for PimError {
    fn from(_: InvalidIdError) -> Self {
        Self::NotFound
    }
}

impl IntoResponse for PimError {
    fn into_response(self) -> axum::response::Response {
        // Internal variants may carry SQL detail or upstream error text: log
        // the full error server-side, answer "internal error" to the client.
        let (status, message, code) = match &self {
            PimError::NotFound | PimError::AccountNotFound => {
                (StatusCode::NOT_FOUND, self.to_string(), Some("not_found"))
            }
            PimError::Unauthorized => (
                StatusCode::UNAUTHORIZED,
                self.to_string(),
                Some("unauthorized"),
            ),
            PimError::Database(_) | PimError::SyncError(_) => {
                tracing::error!(error = %self, "pim request failed");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal error".to_string(),
                    Some("internal_error"),
                )
            }
        };
        (status, Json(ApiErrorBody::new(message, code))).into_response()
    }
}

// ── SeaORM helpers (entity queries on `db.orm()`) ─────────────────────
//
// Ids are TEXT on SQLite and native UUID on Postgres; `IdParam` keeps the
// parse semantics the macro layer used (any text on SQLite, strict UUID on
// Postgres) and these helpers carry them into entity-built statements.

/// Unwrap the driver error SeaORM wraps so `PimError::Database` keeps
/// reporting the underlying `sqlx::Error`; non-driver SeaORM errors become
/// `sqlx::Error::Protocol` with the original message.
fn orm_err(err: sea_orm::DbErr) -> PimError {
    use sea_orm::RuntimeErr;
    let sqlx_err = match err {
        sea_orm::DbErr::Exec(RuntimeErr::SqlxError(e))
        | sea_orm::DbErr::Query(RuntimeErr::SqlxError(e))
        | sea_orm::DbErr::Conn(RuntimeErr::SqlxError(e)) => std::sync::Arc::try_unwrap(e)
            .unwrap_or_else(|shared| sqlx::Error::Protocol(shared.to_string())),
        other => sqlx::Error::Protocol(other.to_string()),
    };
    PimError::Database(sqlx_err)
}

/// Dialect-aware bind for a UUID-column value.
fn id_value(db: &DbPool, id: &str) -> Result<Value, PimError> {
    Ok(match id_param(db, id)? {
        IdParam::Text(s) => Value::String(Some(s)),
        IdParam::Uuid(u) => Value::Uuid(Some(u)),
    })
}

/// Timestamp-shaped bind for `dtstart` / `dtend` (WHERE and SET alike):
/// raw text on SQLite, parsed UTC on Postgres; absent or unparseable input
/// binds a typed NULL, which disables the filter in WHERE and stores NULL.
pub(crate) fn ts_value(db: &DbPool, raw: Option<&str>) -> Value {
    match opt_ts_param(db, raw) {
        Some(TsParam::Text(s)) => Value::String(Some(s)),
        Some(TsParam::Utc(dt)) => Value::ChronoDateTimeUtc(Some(dt)),
        // SQLite never lands here with text in hand (any raw text binds as
        // is), so this arm is the dialect-correct typed NULL for Postgres —
        // and the plain NULL for a missing SQLite value.
        None => match db {
            DbPool::Sqlite(_) => Value::String(None),
            #[cfg(feature = "postgres")]
            DbPool::Postgres(_) => Value::ChronoDateTimeUtc(None),
        },
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

fn missing_column(col: &str) -> PimError {
    PimError::Database(sqlx::Error::Protocol(format!("missing column {col}")))
}

/// Nullable UUID/TEXT id column: `String` on SQLite, native UUID on Postgres.
fn row_opt_id(row: &QueryResult, col: &str) -> Result<Option<String>, PimError> {
    if let Ok(text) = row.try_get::<Option<String>>("", col) {
        return Ok(text);
    }
    row.try_get::<Option<uuid::Uuid>>("", col)
        .map(|opt| opt.map(|u| u.to_string()))
        .map_err(orm_err)
}

/// Decode a UUID/TEXT id column: `String` on SQLite, native UUID on Postgres.
fn row_id(row: &QueryResult, col: &str) -> Result<String, PimError> {
    row_opt_id(row, col)?.ok_or_else(|| missing_column(col))
}

/// Nullable timestamp column: RFC3339 UTC on both backends (SQLite text is
/// zone-less UTC — normalized here so browsers don't read it as local).
fn row_opt_ts(row: &QueryResult, col: &str) -> Result<Option<String>, PimError> {
    if let Ok(text) = row.try_get::<Option<String>>("", col) {
        return Ok(text.map(crate::db_row::normalize_ts_text));
    }
    row.try_get::<Option<DateTime<Utc>>>("", col)
        .map(|opt| opt.map(|t| t.to_rfc3339()))
        .map_err(orm_err)
}

fn row_ts(row: &QueryResult, col: &str) -> Result<String, PimError> {
    row_opt_ts(row, col)?.ok_or_else(|| missing_column(col))
}

/// Ids of the mail accounts owned by `user` — the scope the old
/// `JOIN mail_account … WHERE a.user_id = ?` enforced.
fn accounts_of_user(user: Value) -> SelectStatement {
    let mut sub = Sq::select();
    sub.column(mail_account::Column::Id)
        .from(mail_account::Entity)
        .and_where(mail_account::Column::UserId.eq(user));
    sub
}

/// JSON array column → `Vec<String>`; corrupt or missing JSON yields `[]`.
fn json_array(row: &QueryResult, col: &str) -> Vec<String> {
    row.try_get::<Option<serde_json::Value>>("", col)
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default()
}

fn contact_from_row(row: &QueryResult) -> Result<Contact, PimError> {
    Ok(Contact {
        id: row_id(row, "id")?,
        account_id: row_id(row, "account_id")?,
        display_name: row.try_get("", "display_name").map_err(orm_err)?,
        email_addresses: json_array(row, "email_addresses"),
        phone_numbers: json_array(row, "phone_numbers"),
        organisation: row.try_get("", "organisation").map_err(orm_err)?,
        photo_path: row.try_get("", "photo_path").map_err(orm_err)?,
        created_at: row_ts(row, "created_at")?,
        updated_at: row_ts(row, "updated_at")?,
    })
}

fn calendar_from_row(row: &QueryResult) -> Result<Calendar, PimError> {
    Ok(Calendar {
        id: row_id(row, "id")?,
        account_id: row_id(row, "account_id")?,
        name: row.try_get("", "name").map_err(orm_err)?,
        color: row.try_get("", "color").map_err(orm_err)?,
        description: row.try_get("", "description").map_err(orm_err)?,
        timezone: row.try_get("", "timezone").map_err(orm_err)?,
        is_active: row.try_get("", "is_active").map_err(orm_err)?,
        created_at: row_ts(row, "created_at")?,
        updated_at: row_ts(row, "updated_at")?,
    })
}

fn event_from_row(row: &QueryResult) -> Result<CalendarEvent, PimError> {
    Ok(CalendarEvent {
        id: row_id(row, "id")?,
        calendar_id: row_opt_id(row, "calendar_id")?,
        summary: row.try_get("", "summary").map_err(orm_err)?,
        description: row.try_get("", "description").map_err(orm_err)?,
        dtstart: row_opt_ts(row, "dtstart")?,
        dtend: row_opt_ts(row, "dtend")?,
        location: row.try_get("", "location").map_err(orm_err)?,
        is_all_day: row.try_get("", "is_all_day").map_err(orm_err)?,
        status: row.try_get("", "status").map_err(orm_err)?,
        recurrence_rule: row.try_get("", "recurrence_rule").map_err(orm_err)?,
        created_at: row_ts(row, "created_at")?,
        updated_at: row_ts(row, "updated_at")?,
    })
}

fn add_contact_columns(query: &mut SelectStatement) {
    query
        .column(contact::Column::Id)
        .column(contact::Column::AccountId)
        .column(contact::Column::DisplayName)
        .column(contact::Column::EmailAddresses)
        .column(contact::Column::PhoneNumbers)
        .column(contact::Column::Organisation)
        .column(contact::Column::PhotoPath)
        .column(contact::Column::CreatedAt)
        .column(contact::Column::UpdatedAt);
}

fn add_calendar_columns(query: &mut SelectStatement) {
    query
        .column(calendar::Column::Id)
        .column(calendar::Column::AccountId)
        .column(calendar::Column::Name)
        .column(calendar::Column::Color)
        .column(calendar::Column::Description)
        .column(calendar::Column::Timezone)
        .column(calendar::Column::IsActive)
        .column(calendar::Column::CreatedAt)
        .column(calendar::Column::UpdatedAt);
}

fn add_event_columns(query: &mut SelectStatement) {
    query
        .column(calendar_event::Column::Id)
        .column(calendar_event::Column::CalendarId)
        .column(calendar_event::Column::Summary)
        .column(calendar_event::Column::Description)
        .column(calendar_event::Column::Dtstart)
        .column(calendar_event::Column::Dtend)
        .column(calendar_event::Column::Location)
        .column(calendar_event::Column::IsAllDay)
        .column(calendar_event::Column::Status)
        .column(calendar_event::Column::RecurrenceRule)
        .column(calendar_event::Column::CreatedAt)
        .column(calendar_event::Column::UpdatedAt);
}

/// List contacts, optionally filtered by account.
async fn list_contacts(
    State(state): State<AuthState>,
    AuthUser(user_id): AuthUser,
    Query(query): Query<ListContactsQuery>,
) -> Result<Json<Vec<Contact>>, PimError> {
    let db = state.db();
    let limit = query.limit.unwrap_or(100);
    let offset = query.offset.unwrap_or(0);
    let user = id_value(db, &user_id)?;

    let mut stmt = Sq::select();
    add_contact_columns(&mut stmt);
    stmt.from(contact::Entity)
        .and_where(contact::Column::AccountId.in_subquery(accounts_of_user(user.clone())))
        .order_by(contact::Column::DisplayName, Order::Asc);

    if let Some(account_id) = &query.account_id {
        stmt.and_where(contact::Column::AccountId.eq(id_value(db, account_id)?));
    } else if let Some(search) = &query.q {
        use sea_orm::sea_query::extension::postgres::PgExpr as _;
        // Postgres needs a JSONB text cast for the address search; SQLite's
        // LIKE is already ASCII case-insensitive like ILIKE. Match operators
        // are typed (`.like`/`.ilike`), not raw `?` placeholders — Postgres
        // prepared statements require `$N`.
        let (op, email_col) = match db.backend() {
            sea_orm::DbBackend::Postgres => ("ILIKE", "email_addresses::text"),
            _ => ("LIKE", "email_addresses"),
        };
        let pattern = format!("%{search}%");
        let like = |field: String, pat: String| {
            let f = Expr::cust(field);
            if op == "ILIKE" {
                f.ilike(pat)
            } else {
                f.like(pat)
            }
        };
        stmt.cond_where(
            Condition::any()
                .add(like("display_name".to_owned(), pattern.clone()))
                .add(like(email_col.to_owned(), pattern)),
        );
    }

    stmt.limit(u64::try_from(limit).unwrap_or(u64::MAX))
        .offset(u64::try_from(offset).unwrap_or(0));

    let rows = db.orm().query_all(&stmt).await.map_err(orm_err)?;
    let contacts = rows
        .iter()
        .map(contact_from_row)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Json(contacts))
}

/// Get a specific contact by ID.
async fn get_contact(
    State(state): State<AuthState>,
    Path(id): Path<String>,
    AuthUser(user_id): AuthUser,
) -> Result<Json<Contact>, PimError> {
    let db = state.db();
    let id = id_value(db, &id)?;
    let user = id_value(db, &user_id)?;
    let mut stmt = Sq::select();
    add_contact_columns(&mut stmt);
    stmt.from(contact::Entity)
        .and_where(contact::Column::Id.eq(id))
        .and_where(contact::Column::AccountId.in_subquery(accounts_of_user(user)));
    let row = db.orm().query_one(&stmt).await.map_err(orm_err)?;
    let contact = row
        .map(|r| contact_from_row(&r))
        .transpose()?
        .ok_or(PimError::NotFound)?;
    Ok(Json(contact))
}

/// List calendars, optionally filtered by account.
async fn list_calendars(
    State(state): State<AuthState>,
    AuthUser(user_id): AuthUser,
    Query(query): Query<ListCalendarsQuery>,
) -> Result<Json<Vec<Calendar>>, PimError> {
    let db = state.db();
    let user = id_value(db, &user_id)?;

    let mut stmt = Sq::select();
    add_calendar_columns(&mut stmt);
    stmt.from(calendar::Entity)
        .and_where(calendar::Column::AccountId.in_subquery(accounts_of_user(user)))
        .order_by(calendar::Column::Name, Order::Asc);

    if let Some(account_id) = &query.account_id {
        stmt.and_where(calendar::Column::AccountId.eq(id_value(db, account_id)?));
    }

    let rows = db.orm().query_all(&stmt).await.map_err(orm_err)?;
    let calendars = rows
        .iter()
        .map(calendar_from_row)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Json(calendars))
}

/// Get a specific calendar by ID.
async fn get_calendar(
    State(state): State<AuthState>,
    Path(id): Path<String>,
    AuthUser(user_id): AuthUser,
) -> Result<Json<Calendar>, PimError> {
    let db = state.db();
    let id = id_value(db, &id)?;
    let user = id_value(db, &user_id)?;
    let mut stmt = Sq::select();
    add_calendar_columns(&mut stmt);
    stmt.from(calendar::Entity)
        .and_where(calendar::Column::Id.eq(id))
        .and_where(calendar::Column::AccountId.in_subquery(accounts_of_user(user)));
    let row = db.orm().query_one(&stmt).await.map_err(orm_err)?;
    let cal = row
        .map(|r| calendar_from_row(&r))
        .transpose()?
        .ok_or(PimError::NotFound)?;
    Ok(Json(cal))
}

/// List events for a specific calendar.
async fn list_events(
    State(state): State<AuthState>,
    Path(calendar_id): Path<String>,
    AuthUser(user_id): AuthUser,
    Query(query): Query<ListEventsQuery>,
) -> Result<Json<Vec<CalendarEvent>>, PimError> {
    let db = state.db();
    let calendar = id_value(db, &calendar_id)?;
    let user = id_value(db, &user_id)?;

    // Verify calendar belongs to user
    let mut cal = Sq::select();
    cal.column(calendar::Column::Id)
        .from(calendar::Entity)
        .and_where(calendar::Column::Id.eq(calendar.clone()))
        .and_where(calendar::Column::AccountId.in_subquery(accounts_of_user(user)));
    let owned = db.orm().query_one(&cal).await.map_err(orm_err)?;
    if owned.is_none() {
        return Err(PimError::NotFound);
    }

    let mut stmt = Sq::select();
    add_event_columns(&mut stmt);
    stmt.from(calendar_event::Entity)
        .and_where(calendar_event::Column::CalendarId.eq(calendar));
    if query.start.is_some() {
        stmt.and_where(calendar_event::Column::Dtstart.gte(ts_value(db, query.start.as_deref())));
    }
    if query.end.is_some() {
        stmt.and_where(calendar_event::Column::Dtend.lte(ts_value(db, query.end.as_deref())));
    }
    stmt.order_by(calendar_event::Column::Dtstart, Order::Asc);

    let rows = db.orm().query_all(&stmt).await.map_err(orm_err)?;
    let events = rows
        .iter()
        .map(event_from_row)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Json(events))
}

/// Get a specific event by ID.
async fn get_event(
    State(state): State<AuthState>,
    Path(id): Path<String>,
    AuthUser(user_id): AuthUser,
) -> Result<Json<CalendarEvent>, PimError> {
    let db = state.db();
    let id = id_value(db, &id)?;
    let user = id_value(db, &user_id)?;

    // Events are only reachable through a calendar owned by the user —
    // the old INNER JOIN chain (event → calendar → mail_account).
    let mut calendars_of_user = Sq::select();
    calendars_of_user
        .column(calendar::Column::Id)
        .from(calendar::Entity)
        .and_where(calendar::Column::AccountId.in_subquery(accounts_of_user(user)));

    let mut stmt = Sq::select();
    add_event_columns(&mut stmt);
    stmt.from(calendar_event::Entity)
        .and_where(calendar_event::Column::Id.eq(id))
        .and_where(calendar_event::Column::CalendarId.in_subquery(calendars_of_user));
    let row = db.orm().query_one(&stmt).await.map_err(orm_err)?;
    let event = row
        .map(|r| event_from_row(&r))
        .transpose()?
        .ok_or(PimError::NotFound)?;
    Ok(Json(event))
}

/// Sync contacts for an account via CardDAV (when `carddav_url` is set).
#[allow(clippy::too_many_lines)]
async fn sync_contacts(
    State(state): State<AuthState>,
    Path(account_id): Path<String>,
    AuthUser(user_id): AuthUser,
) -> Result<Json<serde_json::Value>, PimError> {
    let db = state.db();
    let account = id_value(db, &account_id)?;
    let user = id_value(db, &user_id)?;

    let mut acct = Sq::select();
    acct.column(Alias::new("carddav_url"))
        .column(mail_account::Column::EmailAddress)
        .from(mail_account::Entity)
        .and_where(mail_account::Column::Id.eq(account.clone()))
        .and_where(mail_account::Column::UserId.eq(user));
    let row = db
        .orm()
        .query_one(&acct)
        .await
        .map_err(orm_err)?
        .ok_or(PimError::AccountNotFound)?;
    let carddav_url: Option<String> = row.try_get("", "carddav_url").map_err(orm_err)?;
    let email: String = row.try_get("", "email_address").map_err(orm_err)?;

    let Some(base_url) = carddav_url.filter(|s| !s.is_empty()) else {
        return Ok(Json(serde_json::json!({
            "status": "skipped",
            "message": "Set carddavUrl on the account to enable CardDAV sync"
        })));
    };

    let (dek, credential_json) =
        crate::auth::AuthState::get_user_dek_and_credential(state.db(), &user_id, &account_id)
            .await
            .map_err(|e| PimError::SyncError(e.to_string()))?;
    let password = crate::imap::decrypt_account_password(&credential_json, &dek)
        .map_err(|e| PimError::SyncError(e.to_string()))?;

    let client = crate::dav::DavClient::new(email, password, &base_url)
        .map_err(|e| PimError::SyncError(e.to_string()))?;

    let data_dir = state.data_dir.clone();
    let photos_account = account_id.clone();
    let store_photo = {
        let data_dir = std::sync::Arc::new(data_dir.clone());
        let photos_account = std::sync::Arc::new(photos_account.clone());
        move |photo: crate::dav::VcardPhoto| {
            let data_dir = data_dir.clone();
            let photos_account = photos_account.clone();
            Box::pin(async move {
                match photo {
                    crate::dav::VcardPhoto::Inline(bytes) => {
                        crate::blobs::store(&data_dir, &photos_account, &bytes)
                            .await
                            .ok()
                    }
                    crate::dav::VcardPhoto::Uri(url) => {
                        match crate::media::fetch_upstream(&url).await {
                            Ok(img) => crate::blobs::store(&data_dir, &photos_account, &img.bytes)
                                .await
                                .ok(),
                            Err(_) => None,
                        }
                    }
                }
            })
                as std::pin::Pin<Box<dyn std::future::Future<Output = Option<String>> + Send>>
        }
    };

    let outcome =
        crate::pim_dav::sync_carddav(db, &client, &account_id, Some(&base_url), &store_photo)
            .await
            .map_err(|e| PimError::SyncError(e.to_string()))?;

    Ok(Json(serde_json::json!({
        "status": "ok",
        "synced": outcome.changed,
        "removed": outcome.removed,
        "collections": outcome.collections,
    })))
}

/// POST /api/v1/accounts/{id}/pim/discover — RFC 6764 discovery for both
/// protocols; persists the found homesets as carddav_url / caldav_url.
async fn pim_discover(
    State(state): State<AuthState>,
    Path(account_id): Path<String>,
    AuthUser(user_id): AuthUser,
) -> Result<Json<serde_json::Value>, PimError> {
    let db = state.db();
    let account = id_value(db, &account_id)?;
    let user = id_value(db, &user_id)?;

    let mut acct = Sq::select();
    acct.column(mail_account::Column::EmailAddress)
        .from(mail_account::Entity)
        .and_where(mail_account::Column::Id.eq(account))
        .and_where(mail_account::Column::UserId.eq(user));
    let row = db
        .orm()
        .query_one(&acct)
        .await
        .map_err(orm_err)?
        .ok_or(PimError::AccountNotFound)?;
    let email: String = row.try_get("", "email_address").map_err(orm_err)?;

    // Bootstrap origin from the mail domain (SRV host discovery is a
    // follow-up; well-known on the mail host covers common providers).
    let domain = email.rsplit('@').next().unwrap_or_default().to_string();
    let (dek, credential_json) =
        crate::auth::AuthState::get_user_dek_and_credential(state.db(), &user_id, &account_id)
            .await
            .map_err(|e| PimError::SyncError(e.to_string()))?;
    let password = crate::imap::decrypt_account_password(&credential_json, &dek)
        .map_err(|e| PimError::SyncError(e.to_string()))?;

    let bootstraps = crate::pim_dav::bootstrap_origins(&domain, &email, &password);
    if bootstraps.len() < 2 {
        return Err(PimError::SyncError("could not build DAV clients".into()));
    }
    let (carddav, caldav) = crate::pim_dav::discover(&bootstraps)
        .await
        .map_err(|e| PimError::SyncError(e.to_string()))?;

    let mut update = Sq::update();
    update
        .table(mail_account::Entity)
        .value(mail_account::Column::CarddavUrl, carddav.clone())
        .value(mail_account::Column::CaldavUrl, caldav.clone())
        .value(mail_account::Column::UpdatedAt, now_value(db))
        .and_where(mail_account::Column::Id.eq(id_value(db, &account_id)?));
    db.orm().execute(&update).await.map_err(orm_err)?;

    Ok(Json(serde_json::json!({
        "status": "ok",
        "carddavUrl": carddav,
        "caldavUrl": caldav,
    })))
}

/// Sync calendars for an account via CalDAV (when `caldav_url` is set).
#[allow(clippy::too_many_lines)]
async fn sync_calendars(
    State(state): State<AuthState>,
    Path(account_id): Path<String>,
    AuthUser(user_id): AuthUser,
) -> Result<Json<serde_json::Value>, PimError> {
    let db = state.db();
    let account = id_value(db, &account_id)?;
    let user = id_value(db, &user_id)?;

    let mut acct = Sq::select();
    acct.column(Alias::new("caldav_url"))
        .column(mail_account::Column::EmailAddress)
        .from(mail_account::Entity)
        .and_where(mail_account::Column::Id.eq(account))
        .and_where(mail_account::Column::UserId.eq(user));
    let row = db
        .orm()
        .query_one(&acct)
        .await
        .map_err(orm_err)?
        .ok_or(PimError::AccountNotFound)?;
    let caldav_url: Option<String> = row.try_get("", "caldav_url").map_err(orm_err)?;
    let email: String = row.try_get("", "email_address").map_err(orm_err)?;

    let Some(base_url) = caldav_url.filter(|s| !s.is_empty()) else {
        return Ok(Json(serde_json::json!({
            "status": "skipped",
            "message": "Set caldavUrl on the account to enable CalDAV sync"
        })));
    };

    let (dek, credential_json) =
        crate::auth::AuthState::get_user_dek_and_credential(state.db(), &user_id, &account_id)
            .await
            .map_err(|e| PimError::SyncError(e.to_string()))?;
    let password = crate::imap::decrypt_account_password(&credential_json, &dek)
        .map_err(|e| PimError::SyncError(e.to_string()))?;

    let client = crate::dav::DavClient::new(email, password, &base_url)
        .map_err(|e| PimError::SyncError(e.to_string()))?;
    let outcome = crate::pim_dav::sync_caldav(db, &client, &account_id, Some(&base_url))
        .await
        .map_err(|e| PimError::SyncError(e.to_string()))?;

    Ok(Json(serde_json::json!({
        "status": "ok",
        "synced": outcome.changed,
        "removed": outcome.removed,
        "collections": outcome.collections,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn internal_pim_errors_are_masked() {
        let err = PimError::Database(sqlx::Error::Protocol(
            "connection to db.internal:5432 refused".into(),
        ));
        let res = err.into_response();
        assert_eq!(res.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body = axum::body::to_bytes(res.into_body(), 4096).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["error"], "internal error");
        assert_eq!(json["code"], "internal_error");

        let err = PimError::SyncError("token=t0psecret user=admin".into());
        let res = err.into_response();
        assert_eq!(res.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body = axum::body::to_bytes(res.into_body(), 4096).await.unwrap();
        let body = std::str::from_utf8(&body).unwrap();
        assert!(!body.contains("t0psecret"));

        // 404s stay descriptive (deliberate API surface).
        let res = PimError::NotFound.into_response();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
        let body = axum::body::to_bytes(res.into_body(), 4096).await.unwrap();
        let body = std::str::from_utf8(&body).unwrap();
        assert!(body.contains("not found"));
    }
}
