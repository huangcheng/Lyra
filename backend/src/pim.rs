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
    routing::get,
};
use serde::{Deserialize, Serialize};
use sqlx::Row;

use crate::api_error::ApiErrorBody;
use crate::auth::{AuthState, AuthUser};
use crate::db_row::{
    InvalidIdError, id_from_row, id_param, json_text_from_row, opt_id_from_row, opt_json_param,
    opt_ts_from_row, opt_ts_param, ts_from_row,
};

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

macro_rules! contact_from_row {
    ($row:expr) => {{
        let emails = json_text_from_row(&$row, "email_addresses");
        let phones = json_text_from_row(&$row, "phone_numbers");
        Contact {
            id: id_from_row(&$row, "id"),
            account_id: id_from_row(&$row, "account_id"),
            display_name: $row.get("display_name"),
            email_addresses: parse_json_array(emails.as_deref()),
            phone_numbers: parse_json_array(phones.as_deref()),
            organisation: $row.get("organisation"),
            photo_path: $row.get("photo_path"),
            created_at: ts_from_row(&$row, "created_at"),
            updated_at: ts_from_row(&$row, "updated_at"),
        }
    }};
}

macro_rules! calendar_from_row {
    ($row:expr) => {{
        Calendar {
            id: id_from_row(&$row, "id"),
            account_id: id_from_row(&$row, "account_id"),
            name: $row.get("name"),
            color: $row.get("color"),
            description: $row.get("description"),
            timezone: $row.get("timezone"),
            is_active: $row.get::<bool, _>("is_active"),
            created_at: ts_from_row(&$row, "created_at"),
            updated_at: ts_from_row(&$row, "updated_at"),
        }
    }};
}

macro_rules! event_from_row {
    ($row:expr) => {{
        CalendarEvent {
            id: id_from_row(&$row, "id"),
            calendar_id: opt_id_from_row(&$row, "calendar_id"),
            summary: $row.get("summary"),
            description: $row.get("description"),
            dtstart: opt_ts_from_row(&$row, "dtstart"),
            dtend: opt_ts_from_row(&$row, "dtend"),
            location: $row.get("location"),
            is_all_day: $row.get::<bool, _>("is_all_day"),
            status: $row.get("status"),
            recurrence_rule: $row.get("recurrence_rule"),
            created_at: ts_from_row(&$row, "created_at"),
            updated_at: ts_from_row(&$row, "updated_at"),
        }
    }};
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
    let user_bind = id_param(db, &user_id)?;

    let contacts = if let Some(account_id) = &query.account_id {
        let account_bind = id_param(db, account_id)?;
        db_fetch_all!(
            db,
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
            |row| contact_from_row!(row),
            &user_bind,
            &account_bind,
            limit,
            offset
        )?
    } else if let Some(search) = &query.q {
        let pattern = format!("%{search}%");
        db_fetch_all!(
            db,
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
            |row| contact_from_row!(row),
            &user_bind,
            &pattern,
            &pattern,
            limit,
            offset
        )?
    } else {
        db_fetch_all!(
            db,
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
            |row| contact_from_row!(row),
            &user_bind,
            limit,
            offset
        )?
    };

    Ok(Json(contacts))
}

/// Get a specific contact by ID.
async fn get_contact(
    State(state): State<AuthState>,
    Path(id): Path<String>,
    AuthUser(user_id): AuthUser,
) -> Result<Json<Contact>, PimError> {
    let db = state.db();
    let id = id_param(db, &id)?;
    let user_id = id_param(db, &user_id)?;
    let contact = db_fetch_optional!(
        db,
        r"
        SELECT c.id, c.account_id, c.display_name, c.email_addresses,
               c.phone_numbers, c.organisation, c.photo_path,
               c.created_at, c.updated_at
        FROM contact c
        JOIN mail_account a ON c.account_id = a.id
        WHERE c.id = ? AND a.user_id = ?
        ",
        |row| contact_from_row!(&row),
        &id,
        &user_id
    )?
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
    let user_bind = id_param(db, &user_id)?;

    let calendars = if let Some(account_id) = &query.account_id {
        let account_bind = id_param(db, account_id)?;
        db_fetch_all!(
            db,
            r"
            SELECT cal.id, cal.account_id, cal.name, cal.description,
                   cal.color, cal.timezone, cal.is_active,
                   cal.created_at, cal.updated_at
            FROM calendar cal
            JOIN mail_account a ON cal.account_id = a.id
            WHERE a.user_id = ? AND cal.account_id = ?
            ORDER BY cal.name
            ",
            |row| calendar_from_row!(row),
            &user_bind,
            &account_bind
        )?
    } else {
        db_fetch_all!(
            db,
            r"
            SELECT cal.id, cal.account_id, cal.name, cal.description,
                   cal.color, cal.timezone, cal.is_active,
                   cal.created_at, cal.updated_at
            FROM calendar cal
            JOIN mail_account a ON cal.account_id = a.id
            WHERE a.user_id = ?
            ORDER BY cal.name
            ",
            |row| calendar_from_row!(row),
            &user_bind
        )?
    };

    Ok(Json(calendars))
}

/// Get a specific calendar by ID.
async fn get_calendar(
    State(state): State<AuthState>,
    Path(id): Path<String>,
    AuthUser(user_id): AuthUser,
) -> Result<Json<Calendar>, PimError> {
    let db = state.db();
    let id = id_param(db, &id)?;
    let user_id = id_param(db, &user_id)?;
    let calendar = db_fetch_optional!(
        db,
        r"
        SELECT cal.id, cal.account_id, cal.name, cal.description,
               cal.color, cal.timezone, cal.is_active,
               cal.created_at, cal.updated_at
        FROM calendar cal
        JOIN mail_account a ON cal.account_id = a.id
        WHERE cal.id = ? AND a.user_id = ?
        ",
        |row| calendar_from_row!(&row),
        &id,
        &user_id
    )?
    .ok_or(PimError::NotFound)?;
    Ok(Json(calendar))
}

/// List events for a specific calendar.
async fn list_events(
    State(state): State<AuthState>,
    Path(calendar_id): Path<String>,
    AuthUser(user_id): AuthUser,
    Query(query): Query<ListEventsQuery>,
) -> Result<Json<Vec<CalendarEvent>>, PimError> {
    let db = state.db();
    let calendar_bind = id_param(db, &calendar_id)?;
    let user_bind = id_param(db, &user_id)?;

    // Verify calendar belongs to user
    let calendar: Option<String> = db_id_optional!(
        db,
        r"
        SELECT cal.id
        FROM calendar cal
        JOIN mail_account a ON cal.account_id = a.id
        WHERE cal.id = ? AND a.user_id = ?
        ",
        &calendar_bind,
        &user_bind
    )?;
    if calendar.is_none() {
        return Err(PimError::NotFound);
    }

    let start = opt_ts_param(db, query.start.as_deref());
    let end = opt_ts_param(db, query.end.as_deref());
    let events = if query.start.is_some() || query.end.is_some() {
        db_fetch_all!(
            db,
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
            |row| event_from_row!(row),
            &calendar_bind,
            &start,
            &start,
            &end,
            &end
        )?
    } else {
        db_fetch_all!(
            db,
            r"
            SELECT id, calendar_id, summary, description, dtstart, dtend,
                   location, is_all_day, status, recurrence_rule,
                   created_at, updated_at
            FROM calendar_event
            WHERE calendar_id = ?
            ORDER BY dtstart
            ",
            |row| event_from_row!(row),
            &calendar_bind
        )?
    };

    Ok(Json(events))
}

/// Get a specific event by ID.
async fn get_event(
    State(state): State<AuthState>,
    Path(id): Path<String>,
    AuthUser(user_id): AuthUser,
) -> Result<Json<CalendarEvent>, PimError> {
    let db = state.db();
    let id = id_param(db, &id)?;
    let user_id = id_param(db, &user_id)?;
    let event = db_fetch_optional!(
        db,
        r"
        SELECT e.id, e.calendar_id, e.summary, e.description, e.dtstart, e.dtend,
               e.location, e.is_all_day, e.status, e.recurrence_rule,
               e.created_at, e.updated_at
        FROM calendar_event e
        JOIN calendar cal ON e.calendar_id = cal.id
        JOIN mail_account a ON cal.account_id = a.id
        WHERE e.id = ? AND a.user_id = ?
        ",
        |row| event_from_row!(&row),
        &id,
        &user_id
    )?
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
    let account_bind = id_param(db, &account_id)?;
    let user_bind = id_param(db, &user_id)?;

    let row = db_fetch_optional!(
        db,
        r"
        SELECT id, email_address, carddav_url
        FROM mail_account
        WHERE id = ? AND user_id = ?
        ",
        |row| {
            let carddav_url: Option<String> = row.get("carddav_url");
            let email: String = row.get("email_address");
            (carddav_url, email)
        },
        &account_bind,
        &user_bind
    )?
    .ok_or(PimError::AccountNotFound)?;

    let (carddav_url, email) = row;
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
    let hrefs = client
        .propfind_hrefs(&base_url)
        .await
        .map_err(|e| PimError::SyncError(e.to_string()))?;

    let mut synced = 0u32;
    for href in hrefs {
        if !href.to_lowercase().contains(".vcf") {
            continue;
        }
        let url = crate::dav::resolve_href(&base_url, &href);
        let Ok(vcard) = client.get_text(&url).await else {
            continue;
        };
        let (display_name, emails, phones, org) = crate::dav::parse_vcard_fields(&vcard);
        let emails_json = serde_json::to_string(&emails).unwrap_or_else(|_| "[]".into());
        let phones_json = serde_json::to_string(&phones).unwrap_or_else(|_| "[]".into());
        let external_id = href.clone();

        let existing: Option<String> = db_id_optional!(
            db,
            "SELECT id FROM contact WHERE account_id = ? AND external_id = ?",
            &account_bind,
            &external_id
        )?;

        if let Some(id) = existing {
            db_execute!(
                db,
                r"
                UPDATE contact SET
                  vcard_blob = ?, display_name = ?, email_addresses = ?,
                  phone_numbers = ?, organisation = ?, addressbook_url = ?,
                  updated_at = datetime('now')
                WHERE id = ?
                ",
                &vcard,
                &display_name,
                opt_json_param(db, Some(emails_json.as_str())),
                opt_json_param(db, Some(phones_json.as_str())),
                &org,
                &base_url,
                &id_param(db, &id)?
            )?;
        } else {
            let id = uuid::Uuid::new_v7(uuid::Timestamp::now(uuid::NoContext)).to_string();
            db_execute!(
                db,
                r"
                INSERT INTO contact (
                  id, account_id, external_id, vcard_blob, display_name,
                  email_addresses, phone_numbers, organisation, addressbook_url
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
                ",
                &id_param(db, &id)?,
                &account_bind,
                &external_id,
                &vcard,
                &display_name,
                opt_json_param(db, Some(emails_json.as_str())),
                opt_json_param(db, Some(phones_json.as_str())),
                &org,
                &base_url
            )?;
        }
        synced += 1;
    }

    Ok(Json(serde_json::json!({
        "status": "ok",
        "synced": synced,
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
    let account_bind = id_param(db, &account_id)?;
    let user_bind = id_param(db, &user_id)?;

    let row = db_fetch_optional!(
        db,
        r"
        SELECT id, email_address, caldav_url
        FROM mail_account
        WHERE id = ? AND user_id = ?
        ",
        |row| {
            let caldav_url: Option<String> = row.get("caldav_url");
            let email: String = row.get("email_address");
            (caldav_url, email)
        },
        &account_bind,
        &user_bind
    )?
    .ok_or(PimError::AccountNotFound)?;

    let (caldav_url, email) = row;
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
    let hrefs = client
        .propfind_hrefs(&base_url)
        .await
        .map_err(|e| PimError::SyncError(e.to_string()))?;

    // Ensure a local calendar row exists for this URL.
    let calendar_id: String = {
        let existing: Option<String> = db_id_optional!(
            db,
            "SELECT id FROM calendar WHERE account_id = ? AND calendar_url = ?",
            &account_bind,
            &base_url
        )?;
        if let Some(id) = existing {
            id
        } else {
            let id = uuid::Uuid::new_v7(uuid::Timestamp::now(uuid::NoContext)).to_string();
            db_execute!(
                db,
                r"
                INSERT INTO calendar (id, account_id, name, calendar_url, is_active)
                VALUES (?, ?, ?, ?, ?)
                ",
                &id_param(db, &id)?,
                &account_bind,
                "Calendar",
                &base_url,
                true
            )?;
            id
        }
    };

    let mut synced = 0u32;
    for href in hrefs {
        let lower = href.to_lowercase();
        let is_ics = std::path::Path::new(&lower)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("ics"));
        if !(is_ics || lower.contains("vevent") || lower.contains("event")) {
            if href.ends_with('/') {
                continue;
            }
            continue;
        }
        let url = crate::dav::resolve_href(&base_url, &href);
        let Ok(ical) = client.get_text(&url).await else {
            continue;
        };
        if !ical.to_uppercase().contains("BEGIN:VEVENT") {
            continue;
        }
        let (summary, description, dtstart, dtend, location, is_all_day) =
            crate::dav::parse_vevent_fields(&ical);
        let external_id = href.clone();

        let existing: Option<String> = db_id_optional!(
            db,
            "SELECT id FROM calendar_event WHERE account_id = ? AND external_id = ?",
            &account_bind,
            &external_id
        )?;

        if let Some(id) = existing {
            db_execute!(
                db,
                r"
                UPDATE calendar_event SET
                  calendar_id = ?, icalendar_blob = ?, summary = ?, description = ?,
                  dtstart = ?, dtend = ?, location = ?, is_all_day = ?,
                  calendar_url = ?, updated_at = datetime('now')
                WHERE id = ?
                ",
                &id_param(db, &calendar_id)?,
                &ical,
                &summary,
                &description,
                opt_ts_param(db, dtstart.as_deref()),
                opt_ts_param(db, dtend.as_deref()),
                &location,
                is_all_day,
                &base_url,
                &id_param(db, &id)?
            )?;
        } else {
            let id = uuid::Uuid::new_v7(uuid::Timestamp::now(uuid::NoContext)).to_string();
            db_execute!(
                db,
                r"
                INSERT INTO calendar_event (
                  id, account_id, calendar_id, external_id, icalendar_blob,
                  summary, description, dtstart, dtend, location, is_all_day, calendar_url
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                ",
                &id_param(db, &id)?,
                &account_bind,
                &id_param(db, &calendar_id)?,
                &external_id,
                &ical,
                &summary,
                &description,
                opt_ts_param(db, dtstart.as_deref()),
                opt_ts_param(db, dtend.as_deref()),
                &location,
                is_all_day,
                &base_url
            )?;
        }
        synced += 1;
    }

    Ok(Json(serde_json::json!({
        "status": "ok",
        "synced": synced,
        "calendarId": calendar_id,
    })))
}

/// Parse a JSON array string into `Vec<String>`.
fn parse_json_array(json: Option<&str>) -> Vec<String> {
    json.and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default()
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
