//! SeaORM entities for the Lyra schema.
//!
//! One module per table (see `docs/specs/2026-08-20-lyra-data-model-spec.md`).
//! Repositories consume these; handlers and the sync engine never touch
//! entities directly.
//!
//! Conventions:
//! - PKs are app-generated UUIDv7 (`Uuid` maps to TEXT on SQLite, native
//!   UUID on PostgreSQL) — except `jobs.id`, which is TEXT in both dialects.
//! - JSON columns (addresses, flags, labels, …) are `Json` (TEXT on SQLite,
//!   JSONB on PostgreSQL).
//! - Timestamps are `DateTimeUtc`.

// Consumed incrementally by the repository rewrites; remove once every
// entity has at least one production user.
#![allow(dead_code)]

pub mod attachment;
pub mod calendar;
pub mod calendar_event;
pub mod contact;
pub mod folder;
pub mod jobs;
pub mod lyra_user;
pub mod mail_account;
pub mod message;
pub mod opengpg_key;
pub mod sync_cursor;
pub mod thread;

#[cfg(test)]
mod tests {
    use sea_orm::{ConnectionTrait, DbBackend, Statement};

    /// Empirically pin how SQLite TEXT timestamps decode into `DateTimeUtc`
    /// — the schema mixes `datetime('now')` text ("YYYY-MM-DD HH:MM:SS") and
    /// RFC3339 writers, so entity timestamp types must tolerate what the
    /// macro layer still writes.
    #[tokio::test]
    async fn sqlite_text_timestamps_decode_as_datetime_utc() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        let conn = sea_orm::DatabaseConnection::from(pool);

        conn.execute_unprepared("CREATE TABLE t (id TEXT PRIMARY KEY, legacy TEXT, rfc3339 TEXT)")
            .await
            .unwrap();
        conn.execute_unprepared(
            "INSERT INTO t VALUES ('a', datetime('now'), '2026-08-27T09:00:00+00:00')",
        )
        .await
        .unwrap();

        let rows = conn
            .query_all_raw(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT legacy, rfc3339 FROM t",
            ))
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);

        let legacy: Result<chrono::DateTime<chrono::Utc>, _> = rows[0].try_get("", "legacy");
        let rfc: Result<chrono::DateTime<chrono::Utc>, _> = rows[0].try_get("", "rfc3339");
        assert!(rfc.is_ok(), "RFC3339 text must decode (got {rfc:?})");
        // Legacy `datetime('now')` text: the format has no offset; whether it
        // decodes decides if a normalization migration is required before
        // entity reads of macro-written columns.
        if let Ok(ts) = legacy {
            println!("legacy datetime('now') decodes as UTC: {ts}");
        } else {
            panic!(
                "legacy datetime('now') text does not decode as DateTimeUtc; \
                 a timestamp normalization migration is required"
            );
        }
    }
}
