//! Dialect-aware bind values for the sea-orm query layer.
//!
//! SQLite stores ids as TEXT (test fixtures use arbitrary strings) and
//! timestamps as UTC text; PostgreSQL uses native UUID / TIMESTAMPTZ.
//! Query builders match on these to bind the right engine type — mirrors
//! [`crate::auth::db::id_bind_value`].

use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};
use sqlx::encode::IsNull;
use sqlx::error::BoxDynError;
use sqlx::{Encode, Type};
use uuid::Uuid;

use crate::storage::DbPool;

/// Invalid id error: a non-UUID string reached a Postgres UUID bind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidIdError;

impl std::fmt::Display for InvalidIdError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("invalid id")
    }
}

impl std::error::Error for InvalidIdError {}

impl From<InvalidIdError> for sqlx::Error {
    fn from(e: InvalidIdError) -> Self {
        sqlx::Error::Protocol(e.to_string())
    }
}

/// Timestamp text that cannot be parsed for a `TIMESTAMPTZ` bind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidTsError;

impl std::fmt::Display for InvalidTsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("invalid timestamp")
    }
}

impl std::error::Error for InvalidTsError {}

/// Bind a UUID-column id: TEXT on SQLite, `Uuid` on Postgres.
///
/// Invalid UUIDs on the Postgres path are [`InvalidIdError`] (map to 400/404).
#[derive(Debug, Clone)]
pub enum IdParam {
    Text(String),
    #[allow(dead_code)] // construction is feature-gated
    Uuid(Uuid),
}

impl IdParam {
    /// Parse a UUID string. Used by Postgres binds and unit tests.
    #[allow(dead_code)]
    pub fn parse_uuid(id: &str) -> Result<Uuid, InvalidIdError> {
        Uuid::parse_str(id).map_err(|_| InvalidIdError)
    }

    /// Dialect-aware bind value. SQLite accepts any text (tests use `"user-1"`).
    pub fn for_db(db: &DbPool, id: &str) -> Result<Self, InvalidIdError> {
        match db {
            DbPool::Sqlite(_) => Ok(Self::Text(id.to_owned())),
            #[cfg(feature = "postgres")]
            DbPool::Postgres(_) => Self::parse_uuid(id).map(Self::Uuid),
        }
    }
}

/// Shorthand for [`IdParam::for_db`].
pub fn id_param(db: &DbPool, id: &str) -> Result<IdParam, InvalidIdError> {
    IdParam::for_db(db, id)
}

impl Type<sqlx::Sqlite> for IdParam {
    fn type_info() -> sqlx::sqlite::SqliteTypeInfo {
        <String as Type<sqlx::Sqlite>>::type_info()
    }
}

impl<'q> Encode<'q, sqlx::Sqlite> for IdParam {
    fn encode_by_ref(
        &self,
        buf: &mut sqlx::sqlite::SqliteArgumentsBuffer,
    ) -> Result<IsNull, BoxDynError> {
        let text = match self {
            Self::Text(s) => s.clone(),
            Self::Uuid(u) => u.to_string(),
        };
        <String as Encode<'q, sqlx::Sqlite>>::encode(text, buf)
    }
}

#[cfg(feature = "postgres")]
impl Type<sqlx::Postgres> for IdParam {
    fn type_info() -> sqlx::postgres::PgTypeInfo {
        <Uuid as Type<sqlx::Postgres>>::type_info()
    }
}

#[cfg(feature = "postgres")]
impl<'q> Encode<'q, sqlx::Postgres> for IdParam {
    fn encode_by_ref(
        &self,
        buf: &mut sqlx::postgres::PgArgumentBuffer,
    ) -> Result<IsNull, BoxDynError> {
        let uuid = match self {
            Self::Uuid(u) => *u,
            Self::Text(s) => Uuid::parse_str(s)?,
        };
        <Uuid as Encode<'q, sqlx::Postgres>>::encode(uuid, buf)
    }
}

/// Bind a timestamp: UTC text on SQLite (`%Y-%m-%d %H:%M:%S`, sortable
/// against legacy `datetime('now')` rows), `DateTime<Utc>` on Postgres.
#[derive(Debug, Clone)]
pub enum TsParam {
    Text(String),
    #[allow(dead_code)]
    Utc(DateTime<Utc>),
}

impl TsParam {
    /// Parse common ISO / RFC3339 / iCal text for a Postgres bind.
    pub fn for_db(db: &DbPool, raw: &str) -> Result<Self, InvalidTsError> {
        match db {
            DbPool::Sqlite(_) => Ok(Self::Text(raw.to_owned())),
            #[cfg(feature = "postgres")]
            DbPool::Postgres(_) => parse_ts(raw).map(Self::Utc).ok_or(InvalidTsError),
        }
    }
}

/// Optional timestamp bind. Unparseable Postgres values become `None` (NULL).
#[must_use]
pub fn opt_ts_param(db: &DbPool, raw: Option<&str>) -> Option<TsParam> {
    let raw = raw?;
    match TsParam::for_db(db, raw) {
        Ok(p) => Some(p),
        Err(_) => match db {
            DbPool::Sqlite(_) => Some(TsParam::Text(raw.to_owned())),
            #[cfg(feature = "postgres")]
            DbPool::Postgres(_) => None,
        },
    }
}

const NAIVE_TS_FMTS: &[&str] = &[
    "%Y-%m-%d %H:%M:%S",
    "%Y-%m-%dT%H:%M:%S",
    "%Y%m%dT%H%M%SZ",
    "%Y%m%dT%H%M%S",
];

/// Parse RFC3339, RFC2822 (IMAP ENVELOPE), `datetime()` text, or compact iCal instants.
/// Invalid input returns `None` — callers store NULL rather than the raw string.
#[must_use]
pub fn parse_ts(raw: &str) -> Option<DateTime<Utc>> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    if let Ok(dt) = DateTime::parse_from_rfc3339(raw) {
        return Some(dt.with_timezone(&Utc));
    }
    if let Ok(dt) = DateTime::parse_from_rfc2822(raw) {
        return Some(dt.with_timezone(&Utc));
    }
    for fmt in NAIVE_TS_FMTS {
        if let Ok(ndt) = NaiveDateTime::parse_from_str(raw, fmt) {
            return Some(ndt.and_utc());
        }
    }
    if let Ok(d) = NaiveDate::parse_from_str(raw, "%Y-%m-%d") {
        return d.and_hms_opt(0, 0, 0).map(|ndt| ndt.and_utc());
    }
    if let Ok(d) = NaiveDate::parse_from_str(raw, "%Y%m%d") {
        return d.and_hms_opt(0, 0, 0).map(|ndt| ndt.and_utc());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uuid_parse_str_roundtrips_to_string() {
        let id = Uuid::now_v7();
        let text = id.to_string();
        let parsed = Uuid::parse_str(&text).expect("parse");
        assert_eq!(parsed, id);
        assert_eq!(parsed.to_string(), text);
        assert_eq!(IdParam::parse_uuid(&text).unwrap(), id);
    }

    #[test]
    fn uuid_parse_str_rejects_non_uuid() {
        assert!(Uuid::parse_str("user-1").is_err());
        assert!(IdParam::parse_uuid("user-1").is_err());
        assert!(IdParam::parse_uuid("not-a-uuid").is_err());
    }

    #[test]
    fn parse_ts_accepts_rfc3339_and_sqlite_datetime() {
        let rfc = parse_ts("2026-08-23T01:12:00+00:00").expect("rfc");
        assert_eq!(
            rfc.format("%Y-%m-%d %H:%M:%S").to_string(),
            "2026-08-23 01:12:00"
        );
        let sql = parse_ts("2026-08-23 01:12:00").expect("sqlite");
        assert_eq!(sql, rfc);
        assert!(parse_ts("not-a-date").is_none());
    }

    #[test]
    fn parse_ts_normalizes_rfc2822_envelope_dates_to_utc() {
        let z = parse_ts("Tue, 18 Aug 2026 16:25:21 +0000").expect("zulu");
        assert_eq!(z.to_rfc3339(), "2026-08-18T16:25:21+00:00");
        let offset = parse_ts("Tue, 18 Aug 2026 12:25:21 -0400").expect("offset");
        assert_eq!(offset.to_rfc3339(), "2026-08-18T16:25:21+00:00");
        assert_eq!(
            parse_ts("2026-08-18T16:25:21Z")
                .expect("already utc")
                .timestamp(),
            z.timestamp()
        );
        assert!(parse_ts("not a date at all").is_none());
        assert!(parse_ts("").is_none());
    }
}
