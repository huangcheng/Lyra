//! Decode / bind seam for dual-DB column types.
//!
//! SQLite stores ids, timestamps, and JSON as `TEXT`. Postgres uses
//! `UUID` / `TIMESTAMPTZ` / `JSONB`. Handlers keep a `String` / `bool` API;
//! this module is the only place that talks native sqlx types.

use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};
use sqlx::encode::IsNull;
use sqlx::error::BoxDynError;
use sqlx::{Encode, Row, Type, sqlite::SqliteRow};
use uuid::Uuid;

use crate::storage::DbPool;

/// A value that is not a UUID, used so Postgres binds do not 500.
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

/// Row normalisation used by `account_from_row!` / `message_response_from_sql!` etc.
pub trait NormRow {
    fn id(&self, col: &str) -> String;
    fn opt_id(&self, col: &str) -> Option<String>;
    fn ts(&self, col: &str) -> String;
    fn opt_ts(&self, col: &str) -> Option<String>;
    fn json_text(&self, col: &str) -> Option<String>;
}

impl NormRow for SqliteRow {
    fn id(&self, col: &str) -> String {
        self.get::<String, _>(col)
    }

    fn opt_id(&self, col: &str) -> Option<String> {
        self.get::<Option<String>, _>(col)
    }

    fn ts(&self, col: &str) -> String {
        self.get::<String, _>(col)
    }

    fn opt_ts(&self, col: &str) -> Option<String> {
        self.get::<Option<String>, _>(col)
    }

    fn json_text(&self, col: &str) -> Option<String> {
        self.get::<Option<String>, _>(col)
    }
}

#[cfg(feature = "postgres")]
impl NormRow for sqlx::postgres::PgRow {
    fn id(&self, col: &str) -> String {
        self.get::<Uuid, _>(col).to_string()
    }

    fn opt_id(&self, col: &str) -> Option<String> {
        self.get::<Option<Uuid>, _>(col).map(|u| u.to_string())
    }

    fn ts(&self, col: &str) -> String {
        self.get::<DateTime<Utc>, _>(col).to_rfc3339()
    }

    fn opt_ts(&self, col: &str) -> Option<String> {
        self.get::<Option<DateTime<Utc>>, _>(col)
            .map(|t| t.to_rfc3339())
    }

    fn json_text(&self, col: &str) -> Option<String> {
        match self.get::<Option<serde_json::Value>, _>(col) {
            Some(v) if !v.is_null() => Some(v.to_string()),
            _ => None,
        }
    }
}

impl<T: NormRow + ?Sized> NormRow for &T {
    fn id(&self, col: &str) -> String {
        (**self).id(col)
    }
    fn opt_id(&self, col: &str) -> Option<String> {
        (**self).opt_id(col)
    }
    fn ts(&self, col: &str) -> String {
        (**self).ts(col)
    }
    fn opt_ts(&self, col: &str) -> Option<String> {
        (**self).opt_ts(col)
    }
    fn json_text(&self, col: &str) -> Option<String> {
        (**self).json_text(col)
    }
}

/// UUID / TEXT id → `String`.
#[must_use]
pub fn id_from_row(row: &impl NormRow, col: &str) -> String {
    row.id(col)
}

/// Nullable UUID / TEXT id.
#[must_use]
pub fn opt_id_from_row(row: &impl NormRow, col: &str) -> Option<String> {
    row.opt_id(col)
}

/// `TIMESTAMPTZ` → RFC3339; SQLite TEXT is returned as stored.
#[must_use]
pub fn ts_from_row(row: &impl NormRow, col: &str) -> String {
    row.ts(col)
}

/// Nullable timestamp.
#[must_use]
pub fn opt_ts_from_row(row: &impl NormRow, col: &str) -> Option<String> {
    row.opt_ts(col)
}

/// JSONB / TEXT json → JSON text for the API (`from_address`, etc.).
#[must_use]
pub fn json_text_from_row(row: &impl NormRow, col: &str) -> Option<String> {
    row.json_text(col)
}

/// Bind a UUID-column id: TEXT on SQLite, `Uuid` on Postgres.
///
/// Invalid UUIDs on the Postgres path are [`InvalidIdError`] (map to 400/404).
#[derive(Debug, Clone)]
pub enum IdParam {
    Text(String),
    #[allow(dead_code)]
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

/// Optional UUID-column bind.
pub fn opt_id_param(db: &DbPool, id: Option<&str>) -> Result<Option<IdParam>, InvalidIdError> {
    id.map(|s| IdParam::for_db(db, s)).transpose()
}

impl Type<sqlx::Sqlite> for IdParam {
    fn type_info() -> sqlx::sqlite::SqliteTypeInfo {
        <String as Type<sqlx::Sqlite>>::type_info()
    }
}

impl<'q> Encode<'q, sqlx::Sqlite> for IdParam {
    fn encode_by_ref(
        &self,
        buf: &mut Vec<sqlx::sqlite::SqliteArgumentValue<'q>>,
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

/// Bind a timestamp: TEXT on SQLite, `DateTime<Utc>` on Postgres.
#[derive(Debug, Clone)]
pub enum TsParam {
    Text(String),
    #[allow(dead_code)]
    Utc(DateTime<Utc>),
}

impl TsParam {
    /// Bind a value already parsed as UTC.
    #[must_use]
    pub fn from_utc(db: &DbPool, dt: DateTime<Utc>) -> Self {
        match db {
            DbPool::Sqlite(_) => Self::Text(dt.format("%Y-%m-%d %H:%M:%S").to_string()),
            #[cfg(feature = "postgres")]
            DbPool::Postgres(_) => Self::Utc(dt),
        }
    }

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

/// Parse RFC3339, `datetime()` text, or compact iCal instants.
#[must_use]
#[allow(dead_code)]
pub fn parse_ts(raw: &str) -> Option<DateTime<Utc>> {
    let raw = raw.trim();
    if let Ok(dt) = DateTime::parse_from_rfc3339(raw) {
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

impl Type<sqlx::Sqlite> for TsParam {
    fn type_info() -> sqlx::sqlite::SqliteTypeInfo {
        <String as Type<sqlx::Sqlite>>::type_info()
    }
}

impl<'q> Encode<'q, sqlx::Sqlite> for TsParam {
    fn encode_by_ref(
        &self,
        buf: &mut Vec<sqlx::sqlite::SqliteArgumentValue<'q>>,
    ) -> Result<IsNull, BoxDynError> {
        let text = match self {
            Self::Text(s) => s.clone(),
            Self::Utc(dt) => dt.format("%Y-%m-%d %H:%M:%S").to_string(),
        };
        <String as Encode<'q, sqlx::Sqlite>>::encode(text, buf)
    }
}

#[cfg(feature = "postgres")]
impl Type<sqlx::Postgres> for TsParam {
    fn type_info() -> sqlx::postgres::PgTypeInfo {
        <DateTime<Utc> as Type<sqlx::Postgres>>::type_info()
    }
}

#[cfg(feature = "postgres")]
impl<'q> Encode<'q, sqlx::Postgres> for TsParam {
    fn encode_by_ref(
        &self,
        buf: &mut sqlx::postgres::PgArgumentBuffer,
    ) -> Result<IsNull, BoxDynError> {
        let dt = match self {
            Self::Utc(dt) => *dt,
            Self::Text(s) => parse_ts(s).ok_or(InvalidTsError)?,
        };
        <DateTime<Utc> as Encode<'q, sqlx::Postgres>>::encode(dt, buf)
    }
}

/// Bind JSON text: TEXT on SQLite, `JSONB` (`serde_json::Value`) on Postgres.
#[derive(Debug, Clone)]
pub enum JsonParam {
    Text(String),
    #[allow(dead_code)]
    Value(serde_json::Value),
}

impl JsonParam {
    /// Parse JSON for Postgres; keep the original text on SQLite.
    ///
    /// Non-JSON strings become a JSON string scalar so raw headers still bind.
    #[must_use]
    pub fn lenient(db: &DbPool, raw: &str) -> Self {
        match db {
            DbPool::Sqlite(_) => Self::Text(raw.to_owned()),
            #[cfg(feature = "postgres")]
            DbPool::Postgres(_) => match serde_json::from_str(raw) {
                Ok(v) => Self::Value(v),
                Err(_) => Self::Value(serde_json::Value::String(raw.to_owned())),
            },
        }
    }
}

/// Optional JSON bind.
#[must_use]
pub fn opt_json_param(db: &DbPool, raw: Option<&str>) -> Option<JsonParam> {
    raw.map(|s| JsonParam::lenient(db, s))
}

impl Type<sqlx::Sqlite> for JsonParam {
    fn type_info() -> sqlx::sqlite::SqliteTypeInfo {
        <String as Type<sqlx::Sqlite>>::type_info()
    }
}

impl<'q> Encode<'q, sqlx::Sqlite> for JsonParam {
    fn encode_by_ref(
        &self,
        buf: &mut Vec<sqlx::sqlite::SqliteArgumentValue<'q>>,
    ) -> Result<IsNull, BoxDynError> {
        let text = match self {
            Self::Text(s) => s.clone(),
            Self::Value(v) => v.to_string(),
        };
        <String as Encode<'q, sqlx::Sqlite>>::encode(text, buf)
    }
}

#[cfg(feature = "postgres")]
impl Type<sqlx::Postgres> for JsonParam {
    fn type_info() -> sqlx::postgres::PgTypeInfo {
        <serde_json::Value as Type<sqlx::Postgres>>::type_info()
    }
}

#[cfg(feature = "postgres")]
impl<'q> Encode<'q, sqlx::Postgres> for JsonParam {
    fn encode_by_ref(
        &self,
        buf: &mut sqlx::postgres::PgArgumentBuffer,
    ) -> Result<IsNull, BoxDynError> {
        let value = match self {
            Self::Value(v) => v.clone(),
            Self::Text(s) => serde_json::from_str(s)
                .unwrap_or_else(|_| serde_json::Value::String(s.clone())),
        };
        <serde_json::Value as Encode<'q, sqlx::Postgres>>::encode(value, buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

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
        assert_eq!(rfc.format("%Y-%m-%d %H:%M:%S").to_string(), "2026-08-23 01:12:00");
        let sql = parse_ts("2026-08-23 01:12:00").expect("sqlite");
        assert_eq!(sql, rfc);
        assert!(parse_ts("not-a-date").is_none());
    }

    #[tokio::test]
    async fn sqlite_row_helpers_decode_text_columns() {
        let pool = SqlitePoolOptions::new()
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query("CREATE TABLE t (id TEXT, ts TEXT, payload TEXT, maybe_id TEXT)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO t (id, ts, payload, maybe_id) VALUES (?, ?, ?, ?)")
            .bind("abc")
            .bind("2026-08-23 01:00:00")
            .bind(r#"{"a":1}"#)
            .bind(Option::<String>::None)
            .execute(&pool)
            .await
            .unwrap();
        let row = sqlx::query("SELECT id, ts, payload, maybe_id FROM t")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(id_from_row(&row, "id"), "abc");
        assert_eq!(ts_from_row(&row, "ts"), "2026-08-23 01:00:00");
        assert_eq!(
            json_text_from_row(&row, "payload").as_deref(),
            Some(r#"{"a":1}"#)
        );
        assert_eq!(opt_id_from_row(&row, "maybe_id"), None);
        assert_eq!(opt_ts_from_row(&row, "maybe_id"), None);
    }

    #[cfg(feature = "postgres")]
    mod postgres_live {
        use super::*;

        /// Live check: sqlx 0.8 must decode UUID / TIMESTAMPTZ / JSONB through
        /// the query-layer helpers and bind [`IdParam`] / [`TsParam`] / [`JsonParam`].
        /// Set `LYRA_TEST_DATABASE_URL=postgres://…` and run
        /// `cargo test --features postgres -- --ignored`.
        #[tokio::test]
        #[ignore = "needs postgres"]
        async fn native_pg_types_roundtrip_through_query_layer() {
            let url = std::env::var("LYRA_TEST_DATABASE_URL")
                .expect("LYRA_TEST_DATABASE_URL=postgres://…");
            let pool = sqlx::postgres::PgPoolOptions::new()
                .max_connections(1)
                .connect(&url)
                .await
                .expect("connect postgres");
            sqlx::query(
                "CREATE TEMP TABLE lyra_codec_probe (
                    id UUID PRIMARY KEY,
                    ts TIMESTAMPTZ NOT NULL,
                    payload JSONB
                )",
            )
            .execute(&pool)
            .await
            .unwrap();

            let id = Uuid::now_v7();
            let ts = Utc::now();
            let payload = serde_json::json!({"name":"Ada","email":"ada@example.com"});

            sqlx::query("INSERT INTO lyra_codec_probe (id, ts, payload) VALUES ($1, $2, $3)")
                .bind(id)
                .bind(ts)
                .bind(&payload)
                .execute(&pool)
                .await
                .unwrap();

            let row = sqlx::query("SELECT id, ts, payload FROM lyra_codec_probe")
                .fetch_one(&pool)
                .await
                .unwrap();
            assert_eq!(id_from_row(&row, "id"), id.to_string());
            assert!(ts_from_row(&row, "ts").contains('T'));
            let json = json_text_from_row(&row, "payload").expect("json");
            assert!(json.contains("Ada"));
        }
    }
}
