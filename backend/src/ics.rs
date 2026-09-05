//! ICS / webcal subscription fetch + parse.

#![allow(clippy::doc_markdown)]

use chrono::{TimeZone, Utc};
use sea_orm::sea_query::{Expr, Query as Sq};
use sea_orm::{ColumnTrait, ConnectionTrait, Value};

use crate::dav::parse_vevent_fields;
use crate::entities::{calendar_subscription, subscription_event};
use crate::storage::DbPool;
use crate::sync::SyncError;

const MAX_ICS_BYTES: u64 = 2 * 1024 * 1024; // 2 MiB
const REFRESH_EVERY_SECS: i64 = 6 * 60 * 60;

/// Rewrite `webcal://` → `https://` and require a public https URL.
pub fn normalize_ics_url(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("URL is empty".into());
    }
    let lower = trimmed.to_ascii_lowercase();
    let https = if lower.starts_with("webcal://") {
        format!("https://{}", &trimmed["webcal://".len()..])
    } else if lower.starts_with("webcals://") {
        format!("https://{}", &trimmed["webcals://".len()..])
    } else {
        trimmed.to_string()
    };
    crate::netsec::validate_server_url(&https)?;
    let parsed = reqwest::Url::parse(&https).map_err(|e| e.to_string())?;
    if parsed.scheme() != "https" {
        return Err("ICS subscriptions require https:// (or webcal://)".into());
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| "ICS URL has no host".to_string())?;
    if crate::netsec::host_is_local(host) {
        return Err("ICS subscriptions cannot target private or loopback hosts".into());
    }
    Ok(https)
}

#[derive(Debug, Clone)]
pub struct ParsedIcsEvent {
    pub uid: String,
    pub summary: Option<String>,
    pub description: Option<String>,
    pub dtstart: Option<String>,
    pub dtend: Option<String>,
    pub location: Option<String>,
    pub is_all_day: bool,
    pub rrule: Option<String>,
    pub status: Option<String>,
    pub blob: String,
}

#[derive(Debug, Clone)]
pub struct ParsedIcsFeed {
    pub calendar_name: Option<String>,
    pub events: Vec<ParsedIcsEvent>,
}

/// Split a VCALENDAR into VEVENT blobs and extract X-WR-CALNAME.
pub fn parse_ics_feed(ics: &str) -> ParsedIcsFeed {
    let unfolded = unfold_ical(ics);
    let mut calendar_name = None;
    let mut events = Vec::new();
    let mut in_event = false;
    let mut event_lines: Vec<String> = Vec::new();

    for line in unfolded.lines() {
        let trimmed = line.trim_end();
        if trimmed.eq_ignore_ascii_case("BEGIN:VEVENT") {
            in_event = true;
            event_lines.clear();
            event_lines.push(trimmed.to_string());
            continue;
        }
        if trimmed.eq_ignore_ascii_case("END:VEVENT") {
            event_lines.push(trimmed.to_string());
            let blob = event_lines.join("\n");
            if let Some(ev) = parse_one_vevent(&blob) {
                events.push(ev);
            }
            in_event = false;
            event_lines.clear();
            continue;
        }
        if in_event {
            event_lines.push(trimmed.to_string());
            continue;
        }
        if let Some((name_part, value)) = trimmed.split_once(':') {
            let prop = name_part.split(';').next().unwrap_or("").to_uppercase();
            if prop == "X-WR-CALNAME" {
                calendar_name = Some(value.trim().to_string());
            }
        }
    }

    ParsedIcsFeed {
        calendar_name,
        events,
    }
}

fn unfold_ical(ics: &str) -> String {
    let mut out = String::with_capacity(ics.len());
    for raw in ics.split("\r\n").flat_map(|l| l.split('\n')) {
        if raw.starts_with([' ', '\t']) {
            out.push_str(raw.trim_start());
        } else {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(raw);
        }
    }
    out
}

fn parse_one_vevent(blob: &str) -> Option<ParsedIcsEvent> {
    let (summary, description, dtstart_raw, dtend_raw, location, is_all_day) =
        parse_vevent_fields(blob);
    let mut uid = None;
    let mut rrule = None;
    let mut status = None;
    for line in blob.lines() {
        let Some((name_part, value)) = line.split_once(':') else {
            continue;
        };
        let prop = name_part.split(';').next().unwrap_or("").to_uppercase();
        match prop.as_str() {
            "UID" => uid = Some(value.trim().to_string()),
            "RRULE" => rrule = Some(value.trim().to_string()),
            "STATUS" => status = Some(value.trim().to_string()),
            _ => {}
        }
    }
    let uid = uid.filter(|u| !u.is_empty()).unwrap_or_else(|| {
        format!(
            "lyra-ics-{}",
            dtstart_raw.as_deref().unwrap_or("unknown")
        )
    });
    let dtstart = dtstart_raw
        .as_deref()
        .and_then(normalize_ics_dt);
    let dtend = dtend_raw.as_deref().and_then(normalize_ics_dt);
    Some(ParsedIcsEvent {
        uid,
        summary,
        description,
        dtstart,
        dtend,
        location,
        is_all_day,
        rrule,
        status,
        blob: blob.to_string(),
    })
}

fn normalize_ics_dt(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.len() == 8 && raw.chars().all(|c| c.is_ascii_digit()) {
        let y: i32 = raw[0..4].parse().ok()?;
        let m: u32 = raw[4..6].parse().ok()?;
        let d: u32 = raw[6..8].parse().ok()?;
        let nd = chrono::NaiveDate::from_ymd_opt(y, m, d)?.and_hms_opt(0, 0, 0)?;
        return Some(Utc.from_utc_datetime(&nd).to_rfc3339());
    }
    if raw.len() >= 15 && raw.ends_with('Z') {
        let digits = raw.trim_end_matches('Z');
        if digits.len() >= 15 {
            let y: i32 = digits[0..4].parse().ok()?;
            let mo: u32 = digits[4..6].parse().ok()?;
            let d: u32 = digits[6..8].parse().ok()?;
            let h: u32 = digits[9..11].parse().ok()?;
            let mi: u32 = digits[11..13].parse().ok()?;
            let s: u32 = digits[13..15].parse().ok()?;
            let nd = chrono::NaiveDate::from_ymd_opt(y, mo, d)?.and_hms_opt(h, mi, s)?;
            return Some(Utc.from_utc_datetime(&nd).to_rfc3339());
        }
    }
    chrono::DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|dt| dt.with_timezone(&Utc).to_rfc3339())
}

/// Download + SSRF-guarded fetch of an ICS body.
pub async fn fetch_ics_body(url: &str) -> Result<(String, Option<String>, Option<String>), SyncError> {
    let url = normalize_ics_url(url).map_err(SyncError::InvalidInput)?;
    crate::media::validate_outbound_url(&url).await?;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(30))
        .user_agent("Lyra/0.1 (ICS subscription)")
        .build()
        .map_err(|e| SyncError::Internal(format!("ics client: {e}")))?;
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| SyncError::Internal(format!("ics fetch failed: {e}")))?;
    if !resp.status().is_success() {
        return Err(SyncError::Internal(format!(
            "ics fetch HTTP {}",
            resp.status()
        )));
    }
    let etag = resp
        .headers()
        .get(reqwest::header::ETAG)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let last_mod = resp
        .headers()
        .get(reqwest::header::LAST_MODIFIED)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| SyncError::Internal(format!("ics read: {e}")))?;
    if bytes.len() as u64 > MAX_ICS_BYTES {
        return Err(SyncError::InvalidInput("ICS feed too large".into()));
    }
    // Some hosts (notably Apple holiday calendars) always gzip the body.
    // Reqwest's `gzip` feature usually decompresses via Content-Encoding; this
    // fallback covers mislabeled responses that still start with a gzip header.
    let text = decode_ics_bytes(&bytes)?;
    Ok((text, etag, last_mod))
}

fn decode_ics_bytes(bytes: &[u8]) -> Result<String, SyncError> {
    let plain = if bytes.starts_with(&[0x1f, 0x8b]) {
        use std::io::Read;
        let mut decoder = flate2::read::GzDecoder::new(bytes);
        let mut out = Vec::new();
        decoder
            .read_to_end(&mut out)
            .map_err(|e| SyncError::Internal(format!("ics gzip: {e}")))?;
        if out.len() as u64 > MAX_ICS_BYTES {
            return Err(SyncError::InvalidInput("ICS feed too large".into()));
        }
        out
    } else {
        bytes.to_vec()
    };
    Ok(String::from_utf8_lossy(&plain).into_owned())
}

fn now_value(db: &DbPool) -> Value {
    match db {
        DbPool::Sqlite(_) => {
            Value::String(Some(Utc::now().format("%Y-%m-%d %H:%M:%S").to_string()))
        }
        #[cfg(feature = "postgres")]
        DbPool::Postgres(_) => Value::ChronoDateTimeUtc(Some(Utc::now())),
    }
}

fn ts_bind(db: &DbPool, rfc3339: Option<&str>) -> Value {
    match db {
        DbPool::Sqlite(_) => Value::String(rfc3339.map(str::to_string)),
        #[cfg(feature = "postgres")]
        DbPool::Postgres(_) => match rfc3339.and_then(|s| {
            chrono::DateTime::parse_from_rfc3339(s)
                .ok()
                .map(|d| d.with_timezone(&Utc))
        }) {
            Some(dt) => Value::ChronoDateTimeUtc(Some(dt)),
            None => Value::ChronoDateTimeUtc(None),
        },
    }
}

/// Replace cached events for a subscription from a freshly parsed feed.
pub async fn replace_subscription_events(
    db: &DbPool,
    subscription_id: &str,
    events: &[ParsedIcsEvent],
) -> Result<usize, sea_orm::DbErr> {
    let sub = crate::sync::queries::id_value_pub(db, subscription_id)?;
    let mut del = Sq::delete();
    del.from_table(subscription_event::Entity)
        .and_where(subscription_event::Column::SubscriptionId.eq(sub.clone()));
    db.orm().execute(&del).await?;

    for ev in events {
        let raw_id = uuid::Uuid::new_v7(uuid::Timestamp::now(uuid::NoContext)).to_string();
        let id = crate::sync::queries::id_value_pub(db, &raw_id)?;
        let mut insert = Sq::insert();
        insert
            .into_table(subscription_event::Entity)
            .columns([
                subscription_event::Column::Id,
                subscription_event::Column::SubscriptionId,
                subscription_event::Column::ExternalId,
                subscription_event::Column::IcalendarBlob,
                subscription_event::Column::Summary,
                subscription_event::Column::Description,
                subscription_event::Column::Dtstart,
                subscription_event::Column::Dtend,
                subscription_event::Column::Location,
                subscription_event::Column::IsAllDay,
                subscription_event::Column::RecurrenceRule,
                subscription_event::Column::Status,
            ])
            .values_panic([
                Expr::val(id),
                Expr::val(sub.clone()),
                Expr::val(ev.uid.clone()),
                Expr::val(ev.blob.clone()),
                Expr::val(ev.summary.clone().unwrap_or_default()),
                Expr::val(ev.description.clone().unwrap_or_default()),
                Expr::val(ts_bind(db, ev.dtstart.as_deref())),
                Expr::val(ts_bind(db, ev.dtend.as_deref())),
                Expr::val(ev.location.clone().unwrap_or_default()),
                Expr::val(ev.is_all_day),
                Expr::val(ev.rrule.clone().unwrap_or_default()),
                Expr::val(ev.status.clone().unwrap_or_default()),
            ]);
        db.orm().execute(&insert).await?;
    }
    Ok(events.len())
}

/// Fetch feed, update subscription metadata, replace events.
pub async fn refresh_subscription(
    db: &DbPool,
    subscription_id: &str,
) -> Result<usize, SyncError> {
    let sub_id = crate::sync::queries::id_value_pub(db, subscription_id)
        .map_err(|e| SyncError::Internal(e.to_string()))?;
    let mut q = Sq::select();
    q.column(calendar_subscription::Column::Url)
        .column(calendar_subscription::Column::Name)
        .from(calendar_subscription::Entity)
        .and_where(calendar_subscription::Column::Id.eq(sub_id.clone()));
            let row = db
        .orm()
        .query_one(&q)
        .await
        .map_err(|e| SyncError::Internal(e.to_string()))?
        .ok_or_else(|| SyncError::InvalidInput("subscription not found".into()))?;
    let url: String = row
        .try_get("", "url")
        .map_err(|e| SyncError::Internal(e.to_string()))?;
    let existing_name: String = row
        .try_get("", "name")
        .map_err(|e| SyncError::Internal(e.to_string()))?;

    match fetch_ics_body(&url).await {
        Ok((body, etag, last_mod)) => {
            let feed = parse_ics_feed(&body);
            let name = feed
                .calendar_name
                .filter(|n| !n.is_empty())
                .unwrap_or(existing_name);
            let n = replace_subscription_events(db, subscription_id, &feed.events)
                .await
                .map_err(|e| SyncError::Internal(e.to_string()))?;
            let mut upd = Sq::update();
            upd.table(calendar_subscription::Entity)
                .value(calendar_subscription::Column::Name, name)
                .value(calendar_subscription::Column::Etag, etag.unwrap_or_default())
                .value(
                    calendar_subscription::Column::LastModified,
                    last_mod.unwrap_or_default(),
                )
                .value(calendar_subscription::Column::LastFetchedAt, now_value(db))
                .value(calendar_subscription::Column::LastError, Value::String(None))
                .value(calendar_subscription::Column::UpdatedAt, now_value(db))
                .and_where(calendar_subscription::Column::Id.eq(sub_id));
            db.orm()
                .execute(&upd)
                .await
                .map_err(|e| SyncError::Internal(e.to_string()))?;
            Ok(n)
        }
        Err(e) => {
            let msg = e.to_string();
            let mut upd = Sq::update();
            upd.table(calendar_subscription::Entity)
                .value(calendar_subscription::Column::LastError, msg.clone())
                .value(calendar_subscription::Column::UpdatedAt, now_value(db))
                .and_where(calendar_subscription::Column::Id.eq(sub_id));
            let _ = db.orm().execute(&upd).await;
            Err(e)
        }
    }
}

/// Refresh subscriptions whose last fetch is older than REFRESH_EVERY_SECS (or never).
pub async fn refresh_due_subscriptions(db: &DbPool) -> Result<usize, SyncError> {
    let mut q = Sq::select();
    q.column(calendar_subscription::Column::Id)
        .column(calendar_subscription::Column::LastFetchedAt)
        .from(calendar_subscription::Entity)
        .and_where(calendar_subscription::Column::IsActive.eq(true));
    let rows = db
        .orm()
        .query_all(&q)
        .await
        .map_err(|e| SyncError::Internal(e.to_string()))?;
    let cutoff = Utc::now() - chrono::Duration::seconds(REFRESH_EVERY_SECS);
    let mut n = 0usize;
    for row in rows {
        let id: String = if let Ok(s) = row.try_get::<String>("", "id") {
            s
        } else if let Ok(u) = row.try_get::<uuid::Uuid>("", "id") {
            u.to_string()
        } else {
            continue;
        };
        let fetched_before_cutoff = {
            if let Ok(Some(s)) = row.try_get::<Option<String>>("", "last_fetched_at") {
                chrono::NaiveDateTime::parse_from_str(&s, "%Y-%m-%d %H:%M:%S")
                    .ok()
                    .map(|ndt| Utc.from_utc_datetime(&ndt) < cutoff)
                    .or_else(|| {
                        chrono::DateTime::parse_from_rfc3339(&s)
                            .ok()
                            .map(|d| d.with_timezone(&Utc) < cutoff)
                    })
                    .unwrap_or(true)
            } else if let Ok(Some(t)) =
                row.try_get::<Option<chrono::DateTime<Utc>>>("", "last_fetched_at")
            {
                t < cutoff
            } else {
                true // never fetched
            }
        };
        if fetched_before_cutoff {
            match refresh_subscription(db, &id).await {
                Ok(_) => n += 1,
                Err(error) => tracing::warn!(%error, subscription_id = %id, "ICS refresh failed"),
            }
        }
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_webcal_to_https() {
        let u = normalize_ics_url("webcal://calendar.example.com/pub.ics").unwrap();
        assert_eq!(u, "https://calendar.example.com/pub.ics");
        assert!(normalize_ics_url("http://calendar.example.com/x.ics").is_err());
        assert!(normalize_ics_url("").is_err());
        assert!(normalize_ics_url("https://127.0.0.1/secret.ics").is_err());
        assert!(normalize_ics_url("https://localhost/secret.ics").is_err());
    }

    #[test]
    fn parses_multiple_vevents_and_calname() {
        let ics = "BEGIN:VCALENDAR\nX-WR-CALNAME:Holidays\nBEGIN:VEVENT\nUID:a1\nSUMMARY:New Year\nDTSTART;VALUE=DATE:20260101\nDTEND;VALUE=DATE:20260102\nEND:VEVENT\nBEGIN:VEVENT\nUID:a2\nSUMMARY:Meetup\nDTSTART:20260904T100000Z\nDTEND:20260904T110000Z\nEND:VEVENT\nEND:VCALENDAR\n";
        let feed = parse_ics_feed(ics);
        assert_eq!(feed.calendar_name.as_deref(), Some("Holidays"));
        assert_eq!(feed.events.len(), 2);
        assert_eq!(feed.events[0].summary.as_deref(), Some("New Year"));
        assert!(feed.events[0].is_all_day);
        assert_eq!(feed.events[1].uid, "a2");
        assert!(!feed.events[1].is_all_day);
    }

    #[test]
    fn parses_summary_with_language_param() {
        let ics = "BEGIN:VCALENDAR\nX-WR-CALNAME:中国大陆节假日\nBEGIN:VEVENT\nUID:c1\nSUMMARY;LANGUAGE=zh_CN:元旦\nDTSTART;VALUE=DATE:20260101\nDTEND;VALUE=DATE:20260102\nEND:VEVENT\nEND:VCALENDAR\n";
        let feed = parse_ics_feed(ics);
        assert_eq!(feed.calendar_name.as_deref(), Some("中国大陆节假日"));
        assert_eq!(feed.events[0].summary.as_deref(), Some("元旦"));
        assert!(feed.events[0].is_all_day);
    }

    #[test]
    fn decode_ics_bytes_ungzips_apple_style_payload() {
        use std::io::Write;
        let plain = b"BEGIN:VCALENDAR\nEND:VCALENDAR\n";
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        enc.write_all(plain).unwrap();
        let gz = enc.finish().unwrap();
        let text = decode_ics_bytes(&gz).unwrap();
        assert!(text.starts_with("BEGIN:VCALENDAR"));
        assert_eq!(decode_ics_bytes(plain).unwrap(), "BEGIN:VCALENDAR\nEND:VCALENDAR\n");
    }
}

#[cfg(test)]
mod live_icloud {
    use super::*;

    #[tokio::test]
    #[ignore = "network"]
    async fn fetches_cn_holiday_feed() {
        let (text, etag, _) = fetch_ics_body("webcal://p10-calendars.icloud.com/holiday/CN_zh.ics")
            .await
            .expect("fetch");
        assert!(
            text.starts_with("BEGIN:VCALENDAR"),
            "got prefix {:?}",
            text.chars().take(40).collect::<String>()
        );
        let feed = parse_ics_feed(&text);
        assert_eq!(feed.calendar_name.as_deref(), Some("中国大陆节假日"));
        assert!(feed.events.len() > 100, "events={}", feed.events.len());
        assert!(etag.is_some());
        let new_year = feed
            .events
            .iter()
            .find(|e| e.summary.as_deref() == Some("元旦"))
            .expect("元旦");
        assert!(new_year.is_all_day);
    }
}
