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
    http::{HeaderMap, StatusCode, header},
    response::IntoResponse,
    routing::get,
};
use serde::{Deserialize, Serialize};
use sqlx::Row;

use crate::auth::AuthState;
use crate::storage::DbPool;

/// Routes for PIM endpoints.
pub fn routes() -> Router<AuthState> {
    Router::new()
        // Contacts
        .route("/api/contacts", get(list_contacts))
        .route("/api/contacts/{id}", get(get_contact))
        .route(
            "/api/accounts/{account_id}/contacts/sync",
            get(sync_contacts),
        )
        // Calendars
        .route("/api/calendars", get(list_calendars))
        .route("/api/calendars/{id}", get(get_calendar))
        .route("/api/calendars/{id}/events", get(list_events))
        .route("/api/events/{id}", get(get_event))
        .route(
            "/api/accounts/{account_id}/calendars/sync",
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

impl IntoResponse for PimError {
    fn into_response(self) -> axum::response::Response {
        let status = match self {
            PimError::NotFound | PimError::AccountNotFound => StatusCode::NOT_FOUND,
            PimError::Database(_) | PimError::SyncError(_) => StatusCode::INTERNAL_SERVER_ERROR,
            PimError::Unauthorized => StatusCode::UNAUTHORIZED,
        };
        (
            status,
            Json(serde_json::json!({ "error": self.to_string() })),
        )
            .into_response()
    }
}

/// Extract user ID from auth header and session store.
async fn get_user_id(state: &AuthState, headers: &HeaderMap) -> Result<String, PimError> {
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or(PimError::Unauthorized)?;

    state
        .sessions
        .get_session(token)
        .await
        .ok_or(PimError::Unauthorized)
}

/// Get SQLite pool from `DbPool` enum.
fn get_sqlite_pool(db: &DbPool) -> &sqlx::SqlitePool {
    match db {
        DbPool::Sqlite(pool) => pool,
        #[cfg(feature = "postgres")]
        DbPool::Postgres(_) => panic!("PostgreSQL not supported yet"),
    }
}

/// List contacts, optionally filtered by account.
async fn list_contacts(
    State(state): State<AuthState>,
    headers: HeaderMap,
    Query(query): Query<ListContactsQuery>,
) -> Result<Json<Vec<Contact>>, PimError> {
    let user_id = get_user_id(&state, &headers).await?;
    let pool = get_sqlite_pool(state.db());
    let limit = query.limit.unwrap_or(100);
    let offset = query.offset.unwrap_or(0);

    let rows = if let Some(account_id) = &query.account_id {
        sqlx::query(
            r"
            SELECT c.id, c.account_id, c.display_name, c.email_addresses,
                   c.phone_numbers, c.organisation, c.photo_path,
                   c.created_at, c.updated_at
            FROM contact c
            JOIN mail_account a ON c.account_id = a.id
            WHERE a.user_id = ? AND c.account_id = ?
            ORDER BY c.display_name
            LIMIT ? OFFSET ?
            ",
        )
        .bind(&user_id)
        .bind(account_id)
        .bind(limit)
        .bind(offset)
        .fetch_all(pool)
        .await?
    } else if let Some(search) = &query.q {
        let pattern = format!("%{search}%");
        sqlx::query(
            r"
            SELECT c.id, c.account_id, c.display_name, c.email_addresses,
                   c.phone_numbers, c.organisation, c.photo_path,
                   c.created_at, c.updated_at
            FROM contact c
            JOIN mail_account a ON c.account_id = a.id
            WHERE a.user_id = ? AND (c.display_name LIKE ? OR c.email_addresses LIKE ?)
            ORDER BY c.display_name
            LIMIT ? OFFSET ?
            ",
        )
        .bind(&user_id)
        .bind(&pattern)
        .bind(&pattern)
        .bind(limit)
        .bind(offset)
        .fetch_all(pool)
        .await?
    } else {
        sqlx::query(
            r"
            SELECT c.id, c.account_id, c.display_name, c.email_addresses,
                   c.phone_numbers, c.organisation, c.photo_path,
                   c.created_at, c.updated_at
            FROM contact c
            JOIN mail_account a ON c.account_id = a.id
            WHERE a.user_id = ?
            ORDER BY c.display_name
            LIMIT ? OFFSET ?
            ",
        )
        .bind(&user_id)
        .bind(limit)
        .bind(offset)
        .fetch_all(pool)
        .await?
    };

    let contacts: Vec<Contact> = rows
        .iter()
        .map(|row| Contact {
            id: row.get("id"),
            account_id: row.get("account_id"),
            display_name: row.get("display_name"),
            email_addresses: parse_json_array(row.get("email_addresses")),
            phone_numbers: parse_json_array(row.get("phone_numbers")),
            organisation: row.get("organisation"),
            photo_path: row.get("photo_path"),
            created_at: row.get("created_at"),
            updated_at: row.get("updated_at"),
        })
        .collect();

    Ok(Json(contacts))
}

/// Get a specific contact by ID.
async fn get_contact(
    State(state): State<AuthState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Contact>, PimError> {
    let user_id = get_user_id(&state, &headers).await?;
    let pool = get_sqlite_pool(state.db());

    let row = sqlx::query(
        r"
        SELECT c.id, c.account_id, c.display_name, c.email_addresses,
               c.phone_numbers, c.organisation, c.photo_path,
               c.created_at, c.updated_at
        FROM contact c
        JOIN mail_account a ON c.account_id = a.id
        WHERE c.id = ? AND a.user_id = ?
        ",
    )
    .bind(&id)
    .bind(&user_id)
    .fetch_optional(pool)
    .await?
    .ok_or(PimError::NotFound)?;

    Ok(Json(Contact {
        id: row.get("id"),
        account_id: row.get("account_id"),
        display_name: row.get("display_name"),
        email_addresses: parse_json_array(row.get("email_addresses")),
        phone_numbers: parse_json_array(row.get("phone_numbers")),
        organisation: row.get("organisation"),
        photo_path: row.get("photo_path"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
    }))
}

/// List calendars, optionally filtered by account.
async fn list_calendars(
    State(state): State<AuthState>,
    headers: HeaderMap,
    Query(query): Query<ListCalendarsQuery>,
) -> Result<Json<Vec<Calendar>>, PimError> {
    let user_id = get_user_id(&state, &headers).await?;
    let pool = get_sqlite_pool(state.db());

    let rows = if let Some(account_id) = &query.account_id {
        sqlx::query(
            r"
            SELECT cal.id, cal.account_id, cal.name, cal.description,
                   cal.color, cal.timezone, cal.is_active,
                   cal.created_at, cal.updated_at
            FROM calendar cal
            JOIN mail_account a ON cal.account_id = a.id
            WHERE a.user_id = ? AND cal.account_id = ?
            ORDER BY cal.name
            ",
        )
        .bind(&user_id)
        .bind(account_id)
        .fetch_all(pool)
        .await?
    } else {
        sqlx::query(
            r"
            SELECT cal.id, cal.account_id, cal.name, cal.description,
                   cal.color, cal.timezone, cal.is_active,
                   cal.created_at, cal.updated_at
            FROM calendar cal
            JOIN mail_account a ON cal.account_id = a.id
            WHERE a.user_id = ?
            ORDER BY cal.name
            ",
        )
        .bind(&user_id)
        .fetch_all(pool)
        .await?
    };

    let calendars: Vec<Calendar> = rows
        .iter()
        .map(|row| Calendar {
            id: row.get("id"),
            account_id: row.get("account_id"),
            name: row.get("name"),
            color: row.get("color"),
            description: row.get("description"),
            timezone: row.get("timezone"),
            is_active: row.get::<bool, _>("is_active"),
            created_at: row.get("created_at"),
            updated_at: row.get("updated_at"),
        })
        .collect();

    Ok(Json(calendars))
}

/// Get a specific calendar by ID.
async fn get_calendar(
    State(state): State<AuthState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Calendar>, PimError> {
    let user_id = get_user_id(&state, &headers).await?;
    let pool = get_sqlite_pool(state.db());

    let row = sqlx::query(
        r"
        SELECT cal.id, cal.account_id, cal.name, cal.description,
               cal.color, cal.timezone, cal.is_active,
               cal.created_at, cal.updated_at
        FROM calendar cal
        JOIN mail_account a ON cal.account_id = a.id
        WHERE cal.id = ? AND a.user_id = ?
        ",
    )
    .bind(&id)
    .bind(&user_id)
    .fetch_optional(pool)
    .await?
    .ok_or(PimError::NotFound)?;

    Ok(Json(Calendar {
        id: row.get("id"),
        account_id: row.get("account_id"),
        name: row.get("name"),
        color: row.get("color"),
        description: row.get("description"),
        timezone: row.get("timezone"),
        is_active: row.get::<bool, _>("is_active"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
    }))
}

/// List events for a specific calendar.
async fn list_events(
    State(state): State<AuthState>,
    Path(calendar_id): Path<String>,
    headers: HeaderMap,
    Query(query): Query<ListEventsQuery>,
) -> Result<Json<Vec<CalendarEvent>>, PimError> {
    let user_id = get_user_id(&state, &headers).await?;
    let pool = get_sqlite_pool(state.db());

    // Verify calendar belongs to user
    let _calendar = sqlx::query(
        r"
        SELECT cal.id
        FROM calendar cal
        JOIN mail_account a ON cal.account_id = a.id
        WHERE cal.id = ? AND a.user_id = ?
        ",
    )
    .bind(&calendar_id)
    .bind(&user_id)
    .fetch_optional(pool)
    .await?
    .ok_or(PimError::NotFound)?;

    let rows = if query.start.is_some() || query.end.is_some() {
        // Filter by date range
        sqlx::query(
            r"
            SELECT id, calendar_id, summary, description, dtstart, dtend,
                   location, is_all_day, status, recurrence_rule,
                   created_at, updated_at
            FROM calendar_event
            WHERE calendar_id = ?
              AND (dtstart >= ? OR ? IS NULL)
              AND (dtend <= ? OR ? IS NULL)
            ORDER BY dtstart
            ",
        )
        .bind(&calendar_id)
        .bind(&query.start)
        .bind(&query.start)
        .bind(&query.end)
        .bind(&query.end)
        .fetch_all(pool)
        .await?
    } else {
        sqlx::query(
            r"
            SELECT id, calendar_id, summary, description, dtstart, dtend,
                   location, is_all_day, status, recurrence_rule,
                   created_at, updated_at
            FROM calendar_event
            WHERE calendar_id = ?
            ORDER BY dtstart
            ",
        )
        .bind(&calendar_id)
        .fetch_all(pool)
        .await?
    };

    let events: Vec<CalendarEvent> = rows
        .iter()
        .map(|row| CalendarEvent {
            id: row.get("id"),
            calendar_id: row.get("calendar_id"),
            summary: row.get("summary"),
            description: row.get("description"),
            dtstart: row.get("dtstart"),
            dtend: row.get("dtend"),
            location: row.get("location"),
            is_all_day: row.get::<bool, _>("is_all_day"),
            status: row.get("status"),
            recurrence_rule: row.get("recurrence_rule"),
            created_at: row.get("created_at"),
            updated_at: row.get("updated_at"),
        })
        .collect();

    Ok(Json(events))
}

/// Get a specific event by ID.
async fn get_event(
    State(state): State<AuthState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<CalendarEvent>, PimError> {
    let user_id = get_user_id(&state, &headers).await?;
    let pool = get_sqlite_pool(state.db());

    let row = sqlx::query(
        r"
        SELECT e.id, e.calendar_id, e.summary, e.description, e.dtstart, e.dtend,
               e.location, e.is_all_day, e.status, e.recurrence_rule,
               e.created_at, e.updated_at
        FROM calendar_event e
        JOIN calendar cal ON e.calendar_id = cal.id
        JOIN mail_account a ON cal.account_id = a.id
        WHERE e.id = ? AND a.user_id = ?
        ",
    )
    .bind(&id)
    .bind(&user_id)
    .fetch_optional(pool)
    .await?
    .ok_or(PimError::NotFound)?;

    Ok(Json(CalendarEvent {
        id: row.get("id"),
        calendar_id: row.get("calendar_id"),
        summary: row.get("summary"),
        description: row.get("description"),
        dtstart: row.get("dtstart"),
        dtend: row.get("dtend"),
        location: row.get("location"),
        is_all_day: row.get::<bool, _>("is_all_day"),
        status: row.get("status"),
        recurrence_rule: row.get("recurrence_rule"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
    }))
}

/// Sync contacts for an account via `CardDAV`.
async fn sync_contacts(
    State(state): State<AuthState>,
    Path(account_id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, PimError> {
    let user_id = get_user_id(&state, &headers).await?;
    let pool = get_sqlite_pool(state.db());

    // Verify account belongs to user
    let _account = sqlx::query("SELECT id FROM mail_account WHERE id = ? AND user_id = ?")
        .bind(&account_id)
        .bind(&user_id)
        .fetch_optional(pool)
        .await?
        .ok_or(PimError::AccountNotFound)?;

    // TODO: Implement actual CardDAV sync
    // 1. Discover addressbook URLs via PROPFIND
    // 2. Fetch addressbook-multiget for contacts
    // 3. Parse vCard data and store in contact table

    Ok(Json(serde_json::json!({
        "status": "not_implemented",
        "message": "CardDAV sync not yet implemented"
    })))
}

/// Sync calendars for an account via `CalDAV`.
async fn sync_calendars(
    State(state): State<AuthState>,
    Path(account_id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, PimError> {
    let user_id = get_user_id(&state, &headers).await?;
    let pool = get_sqlite_pool(state.db());

    // Verify account belongs to user
    let _account = sqlx::query("SELECT id FROM mail_account WHERE id = ? AND user_id = ?")
        .bind(&account_id)
        .bind(&user_id)
        .fetch_optional(pool)
        .await?
        .ok_or(PimError::AccountNotFound)?;

    // TODO: Implement actual CalDAV sync
    // 1. Discover calendar URLs via PROPFIND
    // 2. Fetch calendar-multiget for events
    // 3. Parse iCalendar data and store in calendar and calendar_event tables

    Ok(Json(serde_json::json!({
        "status": "not_implemented",
        "message": "CalDAV sync not yet implemented"
    })))
}

/// Parse a JSON array string into `Vec<String>`.
fn parse_json_array(json: Option<&str>) -> Vec<String> {
    json.and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default()
}
