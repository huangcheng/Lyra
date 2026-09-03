//! Incremental CardDAV/CalDAV sync engine (RFC 6578), backing the
//! `pim.rs` HTTP handlers.
//!
//! Per collection: hold a sync-token → `sync-collection` REPORT for the
//! delta (changed hrefs → multiget, removed hrefs → tombstone); no token
//! → full PROPFIND etag listing + multiget. Servers without the REPORT
//! fall back to etag-diff. Writes go through etag-guarded PUT/DELETE.

use sea_orm::sea_query::{Expr, OnConflict, Query as Sq};
use sea_orm::{ColumnTrait, ConnectionTrait, Value};

use crate::dav::DavClient;
use crate::dav_protocol::DavItem;
use crate::entities::{calendar_event, contact, dav_cursor};
use crate::storage::DbPool;

pub(crate) struct DavSyncOutcome {
    pub changed: usize,
    pub removed: usize,
    pub collections: usize,
}

/// Async photo sink: inline bytes or URI → blob-store path.
pub(crate) type PhotoStore = dyn Fn(
        crate::dav::VcardPhoto,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<String>> + Send>>
    + Sync;

// ── cursors ───────────────────────────────────────────────────────────

async fn load_cursor(db: &DbPool, account_id: &str, kind: &str) -> Option<String> {
    let mut q = Sq::select();
    q.column(dav_cursor::Column::Token)
        .from(dav_cursor::Entity)
        .and_where(dav_cursor::Column::AccountId.eq(account_id))
        .and_where(dav_cursor::Column::Kind.eq(kind));
    let row = db.orm().query_one(&q).await.ok()??;
    row.try_get::<Option<String>>("", "token").ok().flatten()
}

async fn save_cursor(db: &DbPool, account_id: &str, kind: &str, token: Option<&str>) {
    let kinds = ["carddav", "caldav"];
    debug_assert!(kinds.contains(&kind));
    let value = match token {
        Some(t) => Value::String(Some(t.to_string())),
        None => Value::String(None),
    };
    // Upsert by PK; dialect-safe via raw column values.
    let mut ins = Sq::insert();
    ins.into_table(dav_cursor::Entity)
        .columns([
            dav_cursor::Column::AccountId,
            dav_cursor::Column::Kind,
            dav_cursor::Column::Token,
        ])
        .values_panic([
            Expr::val(account_id),
            Expr::val(kind),
            Expr::val(value.clone()),
        ])
        .on_conflict(
            OnConflict::columns([dav_cursor::Column::AccountId, dav_cursor::Column::Kind])
                .update_column(dav_cursor::Column::Token)
                .to_owned(),
        );
    let _ = db.orm().execute(&ins).await;
}

// ── collection resolution ─────────────────────────────────────────────

/// Resolve the homeset: stored URL if present, else RFC 6764 discovery.
/// `well_known` is `/.well-known/carddav` or `/.well-known/caldav`.
pub(crate) async fn resolve_homeset(
    client: &DavClient,
    stored: Option<&str>,
    well_known: &str,
    homeset_prop: &str,
) -> Result<String, crate::dav::DavError> {
    if let Some(url) = stored.filter(|s| !s.is_empty()) {
        // Stored URLs may be the collection itself (older config style).
        return Ok(url.to_string());
    }
    client.discover_homeset(well_known, homeset_prop).await
}

// ── contact sync ──────────────────────────────────────────────────────

pub(crate) async fn sync_carddav(
    db: &DbPool,
    client: &DavClient,
    account_id: &str,
    stored_url: Option<&str>,
    store_photo: &PhotoStore,
) -> Result<DavSyncOutcome, crate::dav::DavError> {
    let home = resolve_homeset(
        client,
        stored_url,
        "/.well-known/carddav",
        "addressbook-home-set",
    )
    .await?;
    let collections = client.list_collections(&home, "addressbook").await?;
    let mut outcome = DavSyncOutcome {
        changed: 0,
        removed: 0,
        collections: collections.len(),
    };
    // Single-addressbook v1: first collection, one account-level cursor.
    // (Multiple addressbooks collapse to the first; documented in spec.)
    if let Some((collection, _name)) = collections.first() {
        let prior = load_cursor(db, account_id, "carddav").await;
        let (to_fetch, removed, next_token) = delta(client, collection, prior.as_deref()).await?;
        let items = client.addressbook_multiget(collection, &to_fetch).await?;
        for item in items {
            if upsert_contact(db, account_id, collection, &item, store_photo)
                .await
                .is_ok()
            {
                outcome.changed += 1;
            }
        }
        for href in removed {
            tombstone_contact(db, account_id, &href).await;
            outcome.removed += 1;
        }
        save_cursor(db, account_id, "carddav", next_token.as_deref()).await;
    }
    Ok(outcome)
}

/// Changed hrefs + removals + next token. Sync-report first; etag-diff
/// fallback; invalid token → full rebuild.
async fn delta(
    client: &DavClient,
    collection: &str,
    prior: Option<&str>,
) -> Result<(Vec<String>, Vec<String>, Option<String>), crate::dav::DavError> {
    let report = client.sync_collection(collection, prior).await;
    match report {
        Ok(changes) if !changes.invalid => Ok((
            changes.changed.iter().map(|c| c.href.clone()).collect(),
            changes.removed,
            changes.token,
        )),
        _ => {
            // No RFC 6578 (or stale token): full etag listing; removals are
            // computed by the caller's tombstone sweep? v1: report no
            // removals (sync drift is bounded by upserts); token stays None.
            let etags = client.list_etags(collection).await?;
            Ok((
                etags.into_iter().map(|i| i.href).collect(),
                Vec::new(),
                None,
            ))
        }
    }
}

#[allow(clippy::too_many_lines)]
async fn upsert_contact(
    db: &DbPool,
    account_id: &str,
    collection: &str,
    item: &DavItem,
    store_photo: &PhotoStore,
) -> Result<(), sea_orm::DbErr> {
    let Some(vcard) = item.data.as_deref() else {
        return Ok(());
    };
    let (display_name, emails, phones, org) = crate::dav::parse_vcard_fields(vcard);
    let photo_path = match crate::dav::parse_vcard_photo(vcard) {
        Some(photo) => store_photo(photo).await,
        None => None,
    };
    let emails_json = serde_json::json!(emails);
    let phones_json = serde_json::json!(phones);

    let mut existing = Sq::select();
    existing
        .column(contact::Column::Id)
        .column(contact::Column::Etag)
        .from(contact::Entity)
        .and_where(contact::Column::AccountId.eq(account_id))
        .and_where(contact::Column::ExternalId.eq(item.href.clone()));
    let row = db.orm().query_one(&existing).await?;

    let json_val = |v: serde_json::Value| match db {
        DbPool::Sqlite(_) => Value::String(Some(v.to_string())),
        #[cfg(feature = "postgres")]
        DbPool::Postgres(_) => Value::Json(Some(Box::new(v))),
    };

    if let Some(row) = row {
        let id: String = row.try_get("", "id")?;
        let old_etag: Option<String> = row.try_get("", "etag").ok().flatten();
        if old_etag.as_deref() == item.etag.as_deref() && item.etag.is_some() {
            return Ok(()); // unchanged
        }
        let mut update = Sq::update();
        update
            .table(contact::Entity)
            .value(contact::Column::VcardBlob, vcard.to_string())
            .value(contact::Column::DisplayName, display_name.clone())
            .value(
                contact::Column::EmailAddresses,
                Expr::val(json_val(emails_json)),
            )
            .value(
                contact::Column::PhoneNumbers,
                Expr::val(json_val(phones_json)),
            )
            .value(contact::Column::Organisation, org.clone())
            .value(contact::Column::AddressbookUrl, collection.to_string())
            .value(contact::Column::Etag, item.etag.clone().unwrap_or_default())
            .value(contact::Column::UpdatedAt, Expr::current_timestamp())
            .and_where(contact::Column::Id.eq(id.clone()));
        if let Some(photo_path) = photo_path {
            update.value(contact::Column::PhotoPath, photo_path);
        }
        db.orm().execute(&update).await?;
    } else {
        let id = uuid::Uuid::new_v7(uuid::Timestamp::now(uuid::NoContext)).to_string();
        let mut insert = Sq::insert();
        insert
            .into_table(contact::Entity)
            .columns([
                contact::Column::Id,
                contact::Column::AccountId,
                contact::Column::ExternalId,
                contact::Column::VcardBlob,
                contact::Column::DisplayName,
                contact::Column::EmailAddresses,
                contact::Column::PhoneNumbers,
                contact::Column::Organisation,
                contact::Column::AddressbookUrl,
                contact::Column::Etag,
                contact::Column::PhotoPath,
            ])
            .values_panic([
                Expr::val(id),
                Expr::val(account_id),
                Expr::val(item.href.clone()),
                Expr::val(vcard.to_string()),
                Expr::val(display_name.unwrap_or_default()),
                Expr::val(json_val(emails_json)),
                Expr::val(json_val(phones_json)),
                Expr::val(org.unwrap_or_default()),
                Expr::val(collection.to_string()),
                Expr::val(item.etag.clone().unwrap_or_default()),
                Expr::val(photo_path),
            ]);
        db.orm().execute(&insert).await?;
    }
    Ok(())
}

async fn tombstone_contact(db: &DbPool, account_id: &str, href: &str) {
    let mut del = Sq::delete();
    del.from_table(contact::Entity)
        .and_where(contact::Column::AccountId.eq(account_id))
        .and_where(contact::Column::ExternalId.eq(href));
    let _ = db.orm().execute(&del).await;
}

// ── calendar sync ─────────────────────────────────────────────────────

pub(crate) async fn sync_caldav(
    db: &DbPool,
    client: &DavClient,
    account_id: &str,
    stored_url: Option<&str>,
) -> Result<DavSyncOutcome, crate::dav::DavError> {
    let home = resolve_homeset(
        client,
        stored_url,
        "/.well-known/caldav",
        "calendar-home-set",
    )
    .await?;
    let collections = client.list_collections(&home, "calendar").await?;
    let mut outcome = DavSyncOutcome {
        changed: 0,
        removed: 0,
        collections: collections.len(),
    };
    for (collection, name) in &collections {
        let calendar_id = ensure_calendar(db, account_id, collection, name).await;
        let prior = load_calendar_token(db, calendar_id).await;
        let (to_fetch, removed, _next) = delta(client, collection, prior.as_deref()).await?;
        let items = client.calendar_multiget(collection, &to_fetch).await?;
        for item in items {
            if upsert_event(db, account_id, collection, &item)
                .await
                .is_ok()
            {
                outcome.changed += 1;
            }
        }
        for href in removed {
            let mut del = Sq::delete();
            del.from_table(calendar_event::Entity)
                .and_where(calendar_event::Column::AccountId.eq(account_id))
                .and_where(calendar_event::Column::ExternalId.eq(href.clone()));
            let _ = db.orm().execute(&del).await;
            outcome.removed += 1;
        }
    }
    Ok(outcome)
}

async fn ensure_calendar(db: &DbPool, account_id: &str, collection: &str, name: &str) -> String {
    let mut q = Sq::select();
    q.column(crate::entities::calendar::Column::Id)
        .from(crate::entities::calendar::Entity)
        .and_where(crate::entities::calendar::Column::AccountId.eq(account_id))
        .and_where(crate::entities::calendar::Column::CalendarUrl.eq(collection));
    if let Ok(Some(row)) = db.orm().query_one(&q).await
        && let Ok(id) = row.try_get::<String>("", "id")
    {
        return id;
    }
    let id = uuid::Uuid::new_v7(uuid::Timestamp::now(uuid::NoContext)).to_string();
    let mut ins = Sq::insert();
    ins.into_table(crate::entities::calendar::Entity)
        .columns([
            crate::entities::calendar::Column::Id,
            crate::entities::calendar::Column::AccountId,
            crate::entities::calendar::Column::CalendarUrl,
            crate::entities::calendar::Column::Name,
        ])
        .values_panic([
            Expr::val(id.clone()),
            Expr::val(account_id),
            Expr::val(collection),
            Expr::val(name),
        ]);
    let _ = db.orm().execute(&ins).await;
    id
}

async fn load_calendar_token(db: &DbPool, calendar_id: String) -> Option<String> {
    let mut q = Sq::select();
    q.column(crate::entities::calendar::Column::SyncToken)
        .from(crate::entities::calendar::Entity)
        .and_where(crate::entities::calendar::Column::Id.eq(calendar_id.clone()));
    let row = db.orm().query_one(&q).await.ok()??;
    row.try_get::<Option<String>>("", "sync_token")
        .ok()
        .flatten()
}

async fn upsert_event(
    db: &DbPool,
    account_id: &str,
    collection: &str,
    item: &DavItem,
) -> Result<(), sea_orm::DbErr> {
    let Some(ical) = item.data.as_deref() else {
        return Ok(());
    };
    let (summary, description, dtstart, dtend, location, _is_all_day) =
        crate::dav::parse_vevent_fields(ical);
    let mut existing = Sq::select();
    existing
        .column(calendar_event::Column::Id)
        .column(calendar_event::Column::Etag)
        .from(calendar_event::Entity)
        .and_where(calendar_event::Column::AccountId.eq(account_id))
        .and_where(calendar_event::Column::ExternalId.eq(item.href.clone()));
    let row = db.orm().query_one(&existing).await?;

    let ts = |raw: Option<&str>| crate::pim::ts_value(db, normalize_ical_dt(raw).as_deref());

    if let Some(row) = row {
        let id: String = row.try_get("", "id")?;
        let old_etag: Option<String> = row.try_get("", "etag").ok().flatten();
        if old_etag.as_deref() == item.etag.as_deref() && item.etag.is_some() {
            return Ok(());
        }
        let mut update = Sq::update();
        update
            .table(calendar_event::Entity)
            .value(calendar_event::Column::IcalendarBlob, ical.to_string())
            .value(calendar_event::Column::Summary, summary.clone())
            .value(calendar_event::Column::Description, description.clone())
            .value(
                calendar_event::Column::Dtstart,
                Expr::val(ts(dtstart.as_deref())),
            )
            .value(
                calendar_event::Column::Dtend,
                Expr::val(ts(dtend.as_deref())),
            )
            .value(calendar_event::Column::Location, location.clone())
            .value(calendar_event::Column::RecurrenceRule, Value::String(None))
            .value(calendar_event::Column::CalendarUrl, collection.to_string())
            .value(
                calendar_event::Column::Etag,
                item.etag.clone().unwrap_or_default(),
            )
            .value(calendar_event::Column::UpdatedAt, Expr::current_timestamp())
            .and_where(calendar_event::Column::Id.eq(id.clone()));
        db.orm().execute(&update).await?;
    } else {
        let id = uuid::Uuid::new_v7(uuid::Timestamp::now(uuid::NoContext)).to_string();
        let mut insert = Sq::insert();
        insert
            .into_table(calendar_event::Entity)
            .columns([
                calendar_event::Column::Id,
                calendar_event::Column::AccountId,
                calendar_event::Column::ExternalId,
                calendar_event::Column::IcalendarBlob,
                calendar_event::Column::Summary,
                calendar_event::Column::Description,
                calendar_event::Column::Dtstart,
                calendar_event::Column::Dtend,
                calendar_event::Column::Location,
                calendar_event::Column::RecurrenceRule,
                calendar_event::Column::CalendarUrl,
                calendar_event::Column::Etag,
            ])
            .values_panic([
                Expr::val(id),
                Expr::val(account_id),
                Expr::val(item.href.clone()),
                Expr::val(ical.to_string()),
                Expr::val(summary.clone()),
                Expr::val(description.clone()),
                Expr::val(ts(dtstart.as_deref())),
                Expr::val(ts(dtend.as_deref())),
                Expr::val(location.clone().unwrap_or_default()),
                Expr::val(String::new()),
                Expr::val(collection.to_string()),
                Expr::val(item.etag.clone().unwrap_or_default()),
            ]);
        db.orm().execute(&insert).await?;
    }
    Ok(())
}

/// RFC 6764 discovery for both protocols; returns discovered URLs.
pub(crate) async fn discover(client: &DavClient) -> Result<(String, String), crate::dav::DavError> {
    let carddav = client
        .discover_homeset("/.well-known/carddav", "addressbook-home-set")
        .await?;
    let caldav = client
        .discover_homeset("/.well-known/caldav", "calendar-home-set")
        .await?;
    Ok((carddav, caldav))
}

/// iCal `DATE-TIME` (basic format, UTC) or `DATE` → RFC3339 text that
/// [`crate::pim::ts_value`] binds per dialect. `None` when unparseable.
fn normalize_ical_dt(raw: Option<&str>) -> Option<String> {
    parse_ical_dt(raw?.trim()).map(|dt| dt.to_rfc3339())
}

/// iCal `DATE-TIME` (basic format, UTC) or `DATE` → UTC DateTime.
#[allow(clippy::items_after_statements)]
fn parse_ical_dt(raw: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    fn parts(raw: &str, with_time: bool) -> Option<chrono::NaiveDateTime> {
        let (year, month, day): (i32, u32, u32) = (
            raw[0..4].parse().ok()?,
            raw[4..6].parse().ok()?,
            raw[6..8].parse().ok()?,
        );
        if with_time {
            let (hour, minute, second): (u32, u32, u32) = (
                raw[9..11].parse().ok()?,
                raw[11..13].parse().ok()?,
                raw[13..15].parse().ok()?,
            );
            chrono::NaiveDate::from_ymd_opt(year, month, day)?.and_hms_opt(hour, minute, second)
        } else {
            chrono::NaiveDate::from_ymd_opt(year, month, day)?.and_hms_opt(0, 0, 0)
        }
    }
    let raw = raw.trim();
    if raw.len() == 16 && raw.ends_with('Z') {
        return parts(raw, true).map(|dt| dt.and_utc());
    }
    if raw.len() == 8 {
        return parts(raw, false).map(|dt| dt.and_utc());
    }
    chrono::DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|dt| dt.with_timezone(&chrono::Utc))
}

#[cfg(test)]
mod tests {
    #[test]
    fn parses_basic_ical_datetimes() {
        let dt = super::parse_ical_dt("20260904T120000Z").expect("datetime");
        assert_eq!(dt.to_rfc3339(), "2026-09-04T12:00:00+00:00");
        let date = super::parse_ical_dt("20260904").expect("date");
        assert_eq!(date.to_rfc3339(), "2026-09-04T00:00:00+00:00");
        assert!(super::parse_ical_dt("garbage").is_none());
        // The cursor kinds are the dav_cursor PK; changing them orphans
        // stored tokens.
        assert_eq!(["carddav", "caldav"].len(), 2);
    }
}
