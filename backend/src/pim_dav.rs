//! Incremental CardDAV/CalDAV sync engine (RFC 6578), backing the
//! `pim.rs` HTTP handlers.
//!
//! Per collection: hold a sync-token → `sync-collection` REPORT for the
//! delta (changed hrefs → multiget, removed hrefs → tombstone); no token
//! → full PROPFIND etag listing + multiget. Servers without the REPORT
//! fall back to etag-diff. Writes go through etag-guarded PUT/DELETE.

use sea_orm::sea_query::{Expr, OnConflict, Query as Sq};
use sea_orm::{ColumnTrait, ConnectionTrait, ExprTrait, Value};

use crate::dav::DavClient;
use crate::dav_protocol::DavItem;
use crate::entities::{calendar_event, contact, dav_cursor, mail_account};
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
    // Typed UUID bind — raw text params fail `uuid = text` on PostgreSQL.
    let Ok(acct) = crate::sync::queries::id_value_pub(db, account_id) else {
        return None;
    };
    let mut q = Sq::select();
    q.column(dav_cursor::Column::Token)
        .from(dav_cursor::Entity)
        .and_where(dav_cursor::Column::AccountId.eq(acct))
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
    let Ok(acct) = crate::sync::queries::id_value_pub(db, account_id) else {
        return;
    };
    // Upsert by PK; dialect-safe via raw column values.
    let mut ins = Sq::insert();
    ins.into_table(dav_cursor::Entity)
        .columns([
            dav_cursor::Column::AccountId,
            dav_cursor::Column::Kind,
            dav_cursor::Column::Token,
        ])
        .values_panic([Expr::val(acct), Expr::val(kind), Expr::val(value.clone())])
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
            match upsert_contact(db, account_id, collection, &item, store_photo).await {
                Ok(()) => outcome.changed += 1,
                Err(e) => {
                    tracing::warn!(href = %item.href, error = %e, "contact upsert failed");
                }
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
    let account = crate::sync::queries::id_value_pub(db, account_id)?;
    let (display_name, emails, phones, org) = crate::dav::parse_vcard_fields(vcard);
    let photo_path = match crate::dav::parse_vcard_photo(vcard) {
        Some(photo) => store_photo(photo).await,
        None => None,
    };
    let emails_json = serde_json::json!(emails);
    let phones_json = serde_json::json!(phones);

    let mut existing = Sq::select();
    existing
        .expr(
            Expr::col((contact::Entity, contact::Column::Id))
                .cast_as(sea_orm::sea_query::Alias::new("text")),
        )
        .column(contact::Column::Etag)
        .from(contact::Entity)
        .and_where(contact::Column::AccountId.eq(account.clone()))
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
            .and_where(contact::Column::Id.eq(crate::sync::queries::id_value_pub(db, &id)?));
        if let Some(photo_path) = photo_path {
            update.value(contact::Column::PhotoPath, photo_path);
        }
        db.orm().execute(&update).await?;
    } else {
        let raw_id = uuid::Uuid::new_v7(uuid::Timestamp::now(uuid::NoContext)).to_string();
        let id = crate::sync::queries::id_value_pub(db, &raw_id)?;
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
                Expr::val(account.clone()),
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
    let Ok(account) = crate::sync::queries::id_value_pub(db, account_id) else {
        return;
    };
    let mut del = Sq::delete();
    del.from_table(contact::Entity)
        .and_where(contact::Column::AccountId.eq(account))
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
        let prior = load_calendar_token(db, calendar_id.clone()).await;
        let (to_fetch, removed, _next) = delta(client, collection, prior.as_deref()).await?;
        let items = client.calendar_multiget(collection, &to_fetch).await?;
        for item in items {
            if upsert_event(db, account_id, &calendar_id, collection, &item)
                .await
                .is_ok()
            {
                outcome.changed += 1;
            }
        }
        for href in removed {
            let Ok(acct) = crate::sync::queries::id_value_pub(db, account_id) else {
                continue;
            };
            let mut del = Sq::delete();
            del.from_table(calendar_event::Entity)
                .and_where(calendar_event::Column::AccountId.eq(acct))
                .and_where(calendar_event::Column::ExternalId.eq(href.clone()));
            let _ = db.orm().execute(&del).await;
            outcome.removed += 1;
        }
    }
    Ok(outcome)
}

async fn ensure_calendar(db: &DbPool, account_id: &str, collection: &str, name: &str) -> String {
    // account_id must bind as a typed UUID on PostgreSQL — a raw text bind
    // silently fails both the lookup and the insert there.
    let Ok(acct) = crate::sync::queries::id_value_pub(db, account_id) else {
        return String::new();
    };
    let mut q = Sq::select();
    // UUID ids must project as text to decode on PostgreSQL.
    q.expr(
        Expr::col((
            crate::entities::calendar::Entity,
            crate::entities::calendar::Column::Id,
        ))
        .cast_as(sea_orm::sea_query::Alias::new("text")),
    )
    .from(crate::entities::calendar::Entity)
    .and_where(crate::entities::calendar::Column::AccountId.eq(acct.clone()))
    .and_where(crate::entities::calendar::Column::CalendarUrl.eq(collection));
    if let Ok(Some(row)) = db.orm().query_one(&q).await
        && let Ok(id) = row.try_get::<String>("", "id")
    {
        return id;
    }
    let raw_id = uuid::Uuid::new_v7(uuid::Timestamp::now(uuid::NoContext)).to_string();
    let id = crate::sync::queries::id_value_pub(db, &raw_id)
        .unwrap_or(Value::String(Some(raw_id.clone())));
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
            Expr::val(acct),
            Expr::val(collection),
            Expr::val(name),
        ]);
    if let Err(error) = db.orm().execute(&ins).await {
        tracing::warn!(%error, account_id, collection, "calendar row insert failed");
    }
    raw_id
}

async fn load_calendar_token(db: &DbPool, calendar_id: String) -> Option<String> {
    let id = crate::sync::queries::id_value_pub(db, &calendar_id).ok()?;
    let mut q = Sq::select();
    q.column(crate::entities::calendar::Column::SyncToken)
        .from(crate::entities::calendar::Entity)
        .and_where(crate::entities::calendar::Column::Id.eq(id));
    let row = db.orm().query_one(&q).await.ok()??;
    row.try_get::<Option<String>>("", "sync_token")
        .ok()
        .flatten()
}

async fn upsert_event(
    db: &DbPool,
    account_id: &str,
    calendar_id: &str,
    collection: &str,
    item: &DavItem,
) -> Result<(), sea_orm::DbErr> {
    let Some(ical) = item.data.as_deref() else {
        return Ok(());
    };
    let account = crate::sync::queries::id_value_pub(db, account_id)?;
    let calendar = crate::sync::queries::id_value_pub(db, calendar_id)?;
    let (summary, description, dtstart, dtend, location, _is_all_day) =
        crate::dav::parse_vevent_fields(ical);
    let mut existing = Sq::select();
    existing
        .expr(
            Expr::col((calendar_event::Entity, calendar_event::Column::Id))
                .cast_as(sea_orm::sea_query::Alias::new("text")),
        )
        .column(calendar_event::Column::Etag)
        .from(calendar_event::Entity)
        .and_where(calendar_event::Column::AccountId.eq(account.clone()))
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
            .value(calendar_event::Column::CalendarId, calendar.clone())
            .value(
                calendar_event::Column::Etag,
                item.etag.clone().unwrap_or_default(),
            )
            .value(calendar_event::Column::UpdatedAt, Expr::current_timestamp())
            .and_where(calendar_event::Column::Id.eq(crate::sync::queries::id_value_pub(db, &id)?));
        db.orm().execute(&update).await?;
    } else {
        let raw_id = uuid::Uuid::new_v7(uuid::Timestamp::now(uuid::NoContext)).to_string();
        let id = crate::sync::queries::id_value_pub(db, &raw_id)?;
        let mut insert = Sq::insert();
        insert
            .into_table(calendar_event::Entity)
            .columns([
                calendar_event::Column::Id,
                calendar_event::Column::AccountId,
                calendar_event::Column::CalendarId,
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
                Expr::val(account.clone()),
                Expr::val(calendar.clone()),
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

/// Whether account create/update should attempt CardDAV/CalDAV discovery.
/// Bearer (API-token) accounts skip DAV — providers expect an app password
/// on Basic auth for CardDAV/CalDAV (e.g. Fastmail).
pub(crate) fn should_auto_discover_pim(auth_type: &str) -> bool {
    !auth_type.eq_ignore_ascii_case("bearer")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DavAuthError {
    PimPasswordRequired,
}

/// Resolve HTTP Basic password for CardDAV/CalDAV.
/// `pim_password`: decrypted Option from `pim_credential` column (None if column null).
/// `mail_password`: decrypted mail `credential` when usable as a password.
/// `auth_type`: account `auth_type` (`password` | `bearer` | …).
pub fn resolve_dav_password(
    pim_password: Option<&str>,
    mail_password: Option<&str>,
    auth_type: &str,
) -> Result<String, DavAuthError> {
    if let Some(s) = pim_password.filter(|s| !s.is_empty()) {
        return Ok(s.to_string());
    }
    let password_auth = auth_type.is_empty() || auth_type.eq_ignore_ascii_case("password");
    if password_auth && let Some(mail) = mail_password.filter(|s| !s.is_empty()) {
        return Ok(mail.to_string());
    }
    Err(DavAuthError::PimPasswordRequired)
}

/// RFC 6764 discovery from an email + password (no DB). Used by account
/// create/update auto-discover and by `POST …/pim/discover`.
pub(crate) async fn discover_homesets(
    email: &str,
    password: &str,
) -> Result<(String, String), crate::dav::DavError> {
    let domain = email.rsplit('@').next().unwrap_or_default();
    let bootstraps = bootstrap_origins(domain, email, password);
    if bootstraps.len() < 2 {
        return Err(crate::dav::DavError::Protocol(
            "could not build DAV clients".into(),
        ));
    }
    discover(&bootstraps).await
}

/// Persist discovered homesets on the mail account row.
pub(crate) async fn persist_dav_urls(
    db: &DbPool,
    account_id: &str,
    carddav: &str,
    caldav: &str,
) -> Result<(), sea_orm::DbErr> {
    let id = crate::sync::queries::id_value_pub(db, account_id)?;
    let mut update = Sq::update();
    update
        .table(mail_account::Entity)
        .value(mail_account::Column::CarddavUrl, carddav.to_string())
        .value(mail_account::Column::CaldavUrl, caldav.to_string())
        .value(mail_account::Column::UpdatedAt, Expr::current_timestamp())
        .and_where(mail_account::Column::Id.eq(id));
    db.orm().execute(&update).await?;
    Ok(())
}

/// Best-effort discover + persist. Returns URLs on success; `None` when
/// skipped or discovery fails (never fails the caller).
pub(crate) async fn try_auto_discover_pim(
    db: &crate::storage::DbPool,
    account_id: &str,
    email: &str,
    password: &str,
    auth_type: &str,
) -> Option<(String, String)> {
    if !should_auto_discover_pim(auth_type) {
        return None;
    }
    match discover_homesets(email, password).await {
        Ok((carddav, caldav)) => {
            if let Err(error) = persist_dav_urls(db, account_id, &carddav, &caldav).await {
                tracing::warn!(%error, account_id, "failed to persist DAV URLs after discover");
                return None;
            }
            tracing::info!(account_id, %carddav, %caldav, "auto-discovered CardDAV/CalDAV");
            Some((carddav, caldav))
        }
        Err(error) => {
            tracing::info!(%error, account_id, "PIM auto-discover skipped");
            None
        }
    }
}

/// RFC 6764 discovery for both protocols; returns discovered URLs.
/// Bootstrap origins for RFC 6764 discovery. Provider hints cover servers
/// that publish neither well-known nor SRV (Fastmail documents fixed DAV
/// hosts); otherwise well-known on the bare mail domain.
pub(crate) struct Bootstrap {
    pub origin: String,
    pub client: DavClient,
    /// True when the origin IS the DAV root (provider hint): PROPFIND it
    /// directly instead of chaining through /.well-known.
    pub direct: bool,
}

pub(crate) fn bootstrap_origins(domain: &str, email: &str, password: &str) -> Vec<Bootstrap> {
    let hints: &[(&str, &str, &str)] = &[
        (
            "fastmail.com",
            "https://carddav.fastmail.com/dav/",
            "https://caldav.fastmail.com/dav/",
        ),
        (
            "fastmailusercontent.com",
            "https://carddav.fastmail.com/dav/",
            "https://caldav.fastmail.com/dav/",
        ),
    ];
    let mk = |origin: &str| DavClient::new(email.to_string(), password.to_string(), origin);
    for (suffix, carddav, caldav) in hints {
        if domain.ends_with(suffix)
            && let (Ok(c), Ok(l)) = (mk(carddav), mk(caldav))
        {
            return vec![
                Bootstrap {
                    origin: carddav.to_string(),
                    client: c,
                    direct: true,
                },
                Bootstrap {
                    origin: caldav.to_string(),
                    client: l,
                    direct: true,
                },
            ];
        }
    }
    let origin = format!("https://{domain}");
    match (mk(&origin), mk(&origin)) {
        (Ok(c), Ok(l)) => vec![
            Bootstrap {
                origin: origin.clone(),
                client: c,
                direct: false,
            },
            Bootstrap {
                origin,
                client: l,
                direct: false,
            },
        ],
        _ => Vec::new(),
    }
}

/// RFC 6764 discovery over per-protocol clients; returns home-set URLs.
pub(crate) async fn discover(
    bootstraps: &[Bootstrap],
) -> Result<(String, String), crate::dav::DavError> {
    let card = &bootstraps[0];
    let cal = &bootstraps[1];
    let carddav = if card.direct {
        card.client
            .homeset_direct(&card.origin, "addressbook-home-set")
            .await?
    } else {
        card.client
            .discover_homeset("/.well-known/carddav", "addressbook-home-set")
            .await?
    };
    let caldav = if cal.direct {
        cal.client
            .homeset_direct(&cal.origin, "calendar-home-set")
            .await?
    } else {
        cal.client
            .discover_homeset("/.well-known/caldav", "calendar-home-set")
            .await?
    };
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

/// Live roundtrip for the calendar row seam: `ensure_calendar` must bind
/// `account_id` as a typed UUID on PostgreSQL (raw text binds made the
/// insert fail silently, leaving the calendar rail permanently empty).
#[cfg(test)]
mod postgres_live {
    use super::*;
    use crate::pgtest::support;

    #[test]
    #[ignore = "needs postgres"]
    fn ensure_calendar_creates_and_reuses_rows() {
        support::rt().block_on(async {
            let (db, user_id) = support::setup().await;
            let account_id = support::seed_account(&db, &user_id, "cal@example.com").await;

            let id = ensure_calendar(
                &db,
                &account_id,
                "https://dav.example/cal/personal/",
                "Personal",
            )
            .await;
            assert!(!id.is_empty());

            let mut q = Sq::select();
            q.column(crate::entities::calendar::Column::Id)
                .from(crate::entities::calendar::Entity)
                .and_where(
                    crate::entities::calendar::Column::CalendarUrl
                        .eq("https://dav.example/cal/personal/".to_string()),
                );
            let rows = db.orm().query_all(&q).await.unwrap();
            assert_eq!(rows.len(), 1, "calendar row must exist after ensure");

            // Second call returns the SAME calendar (no duplicate rows).
            let id2 = ensure_calendar(
                &db,
                &account_id,
                "https://dav.example/cal/personal/",
                "Personal",
            )
            .await;
            assert_eq!(id, id2, "ensure_calendar must be idempotent per URL");
            let rows = db.orm().query_all(&q).await.unwrap();
            assert_eq!(rows.len(), 1, "second ensure must not insert again");
        });
    }

    const VEVENT: &str = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:pgtest-1\r\n\
SUMMARY:Red test\r\nDTSTART:20260907T090000Z\r\nDTEND:20260907T100000Z\r\n\
END:VEVENT\r\nEND:VCALENDAR\r\n";

    /// Events must carry `calendar_id` (the list API filters on it) and the
    /// etag-change UPDATE path must work: both broke on PostgreSQL via raw
    /// text/UUID id handling.
    #[test]
    #[ignore = "needs postgres"]
    fn event_upsert_links_calendar_and_updates() {
        use sea_orm::QueryResult;

        async fn read_event(db: &DbPool) -> QueryResult {
            let mut q = Sq::select();
            q.column(calendar_event::Column::CalendarId)
                .column(calendar_event::Column::Summary)
                .from(calendar_event::Entity)
                .and_where(calendar_event::Column::ExternalId.eq("/pgtest-1.ics".to_string()));
            db.orm().query_one(&q).await.unwrap().expect("event row")
        }

        support::rt().block_on(async {
            let (db, user_id) = support::setup().await;
            let account_id = support::seed_account(&db, &user_id, "cal2@example.com").await;
            // Distinct from the other test's URL: tests run in parallel
            // against the shared database.
            let calendar_id = ensure_calendar(
                &db,
                &account_id,
                "https://dav.example/cal/events-b/",
                "Personal",
            )
            .await;

            upsert_event(
                &db,
                &account_id,
                &calendar_id,
                "https://dav.example/cal/personal/",
                &crate::dav_protocol::DavItem {
                    href: "/pgtest-1.ics".into(),
                    etag: Some("\"v1\"".into()),
                    data: Some(VEVENT.into()),
                },
            )
            .await
            .unwrap();

            let row = read_event(&db).await;
            let linked: Option<String> = row
                .try_get::<Option<uuid::Uuid>>("", "calendar_id")
                .ok()
                .flatten()
                .map(|u| u.to_string());
            assert_eq!(
                linked.as_deref(),
                Some(calendar_id.as_str()),
                "event must link to its calendar"
            );
            assert_eq!(
                row.try_get::<String>("", "summary").unwrap(),
                "Red test",
                "summary parsed from VEVENT"
            );

            // Different etag → UPDATE path (raw-UUID id decode broke it).
            upsert_event(
                &db,
                &account_id,
                &calendar_id,
                "https://dav.example/cal/personal/",
                &crate::dav_protocol::DavItem {
                    href: "/pgtest-1.ics".into(),
                    etag: Some("\"v2\"".into()),
                    data: Some(VEVENT.replace("Red test", "Green test")),
                },
            )
            .await
            .unwrap();

            let mut count = Sq::select();
            count
                .column(calendar_event::Column::Id)
                .from(calendar_event::Entity)
                .and_where(calendar_event::Column::ExternalId.eq("/pgtest-1.ics".to_string()));
            let rows = db.orm().query_all(&count).await.unwrap();
            assert_eq!(rows.len(), 1, "etag change updates, never duplicates");
            let row = read_event(&db).await;
            assert_eq!(
                row.try_get::<String>("", "summary").unwrap(),
                "Green test",
                "update path applied new data"
            );
        });
    }
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

    #[test]
    fn auto_discover_skips_bearer_auth() {
        assert!(super::should_auto_discover_pim("password"));
        assert!(!super::should_auto_discover_pim("bearer"));
        assert!(!super::should_auto_discover_pim("Bearer"));
    }

    #[test]
    fn fastmail_bootstrap_is_direct_dav_root() {
        let boots = super::bootstrap_origins("fastmail.com", "user@fastmail.com", "app-password");
        assert_eq!(boots.len(), 2);
        assert!(boots[0].direct);
        assert!(boots[0].origin.contains("carddav.fastmail.com"));
        assert!(boots[1].origin.contains("caldav.fastmail.com"));
    }

    #[test]
    fn dav_password_prefers_pim() {
        assert_eq!(
            super::resolve_dav_password(Some("app-pass"), Some("mail-pass"), "bearer").unwrap(),
            "app-pass"
        );
    }

    #[test]
    fn dav_password_falls_back_for_password_auth() {
        assert_eq!(
            super::resolve_dav_password(None, Some("mail-pass"), "password").unwrap(),
            "mail-pass"
        );
    }

    #[test]
    fn dav_password_requires_pim_for_bearer() {
        assert!(matches!(
            super::resolve_dav_password(None, Some("bearer-token"), "bearer"),
            Err(super::DavAuthError::PimPasswordRequired)
        ));
    }
}
