//! Dashboard stats endpoint: read-only aggregates over local message rows.
//!
//! `GET /api/v1/messages/stats?days=14` feeds the analytics dashboard.
//! Additive and read-only: no sync state, no writes.

use axum::{
    Json, Router,
    extract::{Query, State},
    routing::get,
};
use serde::{Deserialize, Serialize};
use sqlx::Row;

use crate::auth::{AuthState, AuthUser};
use crate::db_row::{IdParam, TsParam, id_param, json_text_from_row};
use crate::privacy::sender_email_from_json;
use crate::storage::DbPool;
use crate::sync::SyncError;

pub fn routes() -> Router<AuthState> {
    Router::new().route("/api/v1/messages/stats", get(message_stats))
}

/// One day of inbound volume.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DailyCount {
    pub date: String,
    pub received: i64,
}

/// One sender row in the top-senders ranking.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SenderCount {
    pub address: String,
    pub name: Option<String>,
    pub count: i64,
}

/// Window totals plus the current unread snapshot.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatsTotals {
    pub received: i64,
    pub sent: i64,
    pub unread: i64,
}

/// Response for `GET /api/v1/messages/stats`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageStatsResponse {
    pub days: i64,
    pub daily: Vec<DailyCount>,
    pub top_senders: Vec<SenderCount>,
    pub totals: StatsTotals,
}

/// Query for `GET /api/v1/messages/stats`.
#[derive(Debug, Deserialize)]
pub(crate) struct StatsQuery {
    days: Option<i64>,
}

/// Window length in days: default 14, clamped to 1..=90.
pub(crate) fn clamp_days(days: Option<i64>) -> i64 {
    days.unwrap_or(14).clamp(1, 90)
}

/// GET /api/v1/messages/stats?days=14 — dashboard aggregates for the user.
///
/// The window is rolling: `days=14` means "now minus 14 days", not the last
/// 14 calendar days, so the earliest daily bucket may be partial. Daily
/// buckets are grouped by UTC date on both engines (Postgres pool sessions
/// are pinned to `TIME ZONE UTC`; see `storage.rs`).
pub(crate) async fn message_stats(
    State(state): State<AuthState>,
    AuthUser(user_id): AuthUser,
    Query(query): Query<StatsQuery>,
) -> Result<Json<MessageStatsResponse>, SyncError> {
    let days = clamp_days(query.days);
    let response = query_message_stats(state.db(), &user_id, days).await?;
    Ok(Json(response))
}

/// Aggregate daily received counts, top senders, and totals for a user.
///
/// "Received" excludes sent/drafts folders and soft-deleted rows; `sent`
/// counts sent-folder rows. Both are bounded to the `days` window, while
/// `unread` is a current inbox snapshot mirroring the `/messages` filters.
pub(crate) async fn query_message_stats(
    db: &DbPool,
    user_id: &str,
    days: i64,
) -> Result<MessageStatsResponse, SyncError> {
    let user_bind = id_param(db, user_id)?;
    let cutoff = TsParam::from_utc(db, chrono::Utc::now() - chrono::Duration::days(days));

    Ok(MessageStatsResponse {
        days,
        daily: query_daily(db, &user_bind, &cutoff).await?,
        top_senders: query_top_senders(db, &user_bind, &cutoff).await?,
        totals: query_totals(db, &user_bind, &cutoff).await?,
    })
}

/// Per-day received counts over the window, ascending by date.
///
/// CAST to TEXT keeps the date decode identical on both engines
/// (SQLite `date()` → TEXT; Postgres `date()` → DATE → text). Bucketing is
/// UTC on both: SQLite dates are UTC text, and Postgres sessions are pinned
/// to `TIME ZONE UTC` at connect (see `storage.rs`).
async fn query_daily(
    db: &DbPool,
    user_bind: &IdParam,
    cutoff: &TsParam,
) -> Result<Vec<DailyCount>, SyncError> {
    db_fetch_all!(
        db,
        r"
        SELECT CAST(date(m.date) AS TEXT) AS d, COUNT(*) AS c
        FROM message m
        JOIN folder f ON m.folder_id = f.id
        JOIN mail_account a ON m.account_id = a.id
        WHERE a.user_id = ?
          AND m.is_deleted = ?
          AND m.date IS NOT NULL
          AND m.date >= ?
          AND (f.role IS NULL OR f.role NOT IN ('sent', 'drafts'))
        GROUP BY d
        ORDER BY d
        ",
        |row| DailyCount {
            date: row.get("d"),
            received: row.get("c"),
        },
        user_bind,
        false,
        cutoff
    )
    .map_err(SyncError::from)
}

/// Top 5 senders by received count over the window.
///
/// Groups by the raw JSON address object so the decode seam
/// (`json_text_from_row`) handles TEXT vs JSONB; name/email extraction
/// happens in Rust.
async fn query_top_senders(
    db: &DbPool,
    user_bind: &IdParam,
    cutoff: &TsParam,
) -> Result<Vec<SenderCount>, SyncError> {
    let rows = db_fetch_all!(
        db,
        r"
        SELECT m.from_address AS fa, COUNT(*) AS c
        FROM message m
        JOIN folder f ON m.folder_id = f.id
        JOIN mail_account a ON m.account_id = a.id
        WHERE a.user_id = ?
          AND m.is_deleted = ?
          AND m.date IS NOT NULL
          AND m.date >= ?
          AND (f.role IS NULL OR f.role NOT IN ('sent', 'drafts'))
          AND m.from_address IS NOT NULL
        GROUP BY m.from_address
        ORDER BY c DESC
        LIMIT 5
        ",
        |row| (json_text_from_row(row, "fa"), row.get::<i64, _>("c")),
        user_bind,
        false,
        cutoff
    )?;

    Ok(rows
        .into_iter()
        .filter_map(|(from_json, count)| {
            let address = sender_email_from_json(from_json.as_deref())?;
            Some(SenderCount {
                address,
                name: sender_name_from_json(from_json.as_deref()),
                count,
            })
        })
        .collect())
}

/// Window totals for received/sent plus the current unread inbox snapshot.
async fn query_totals(
    db: &DbPool,
    user_bind: &IdParam,
    cutoff: &TsParam,
) -> Result<StatsTotals, SyncError> {
    let received = db_scalar!(
        db,
        i64,
        r"
        SELECT COUNT(*)
        FROM message m
        JOIN folder f ON m.folder_id = f.id
        JOIN mail_account a ON m.account_id = a.id
        WHERE a.user_id = ?
          AND m.is_deleted = ?
          AND m.date IS NOT NULL
          AND m.date >= ?
          AND (f.role IS NULL OR f.role NOT IN ('sent', 'drafts'))
        ",
        user_bind,
        false,
        cutoff
    )?;

    let sent = db_scalar!(
        db,
        i64,
        r"
        SELECT COUNT(*)
        FROM message m
        JOIN folder f ON m.folder_id = f.id
        JOIN mail_account a ON m.account_id = a.id
        WHERE a.user_id = ?
          AND m.is_deleted = ?
          AND m.date IS NOT NULL
          AND m.date >= ?
          AND f.role = 'sent'
        ",
        user_bind,
        false,
        cutoff
    )?;

    // Unread snapshot: inbox role, read flag unset, not deleted, not snoozed —
    // the same visibility filters the /messages list handlers apply.
    let unread = db_scalar!(
        db,
        i64,
        r"
        SELECT COUNT(*)
        FROM message m
        JOIN folder f ON m.folder_id = f.id
        JOIN mail_account a ON m.account_id = a.id
        WHERE a.user_id = ?
          AND f.role = 'inbox'
          AND m.is_read = ?
          AND m.is_deleted = ?
          AND (m.snoozed_until IS NULL OR m.snoozed_until <= datetime('now'))
        ",
        user_bind,
        false,
        false
    )?;

    Ok(StatsTotals {
        received,
        sent,
        unread,
    })
}

/// Display name from a `from_address` JSON object (`{"name": …}`), if present.
fn sender_name_from_json(from_address: Option<&str>) -> Option<String> {
    let parsed: serde_json::Value = serde_json::from_str(from_address?).ok()?;
    parsed
        .get("name")
        .and_then(|v| v.as_str())
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db_row::{message_date_param, opt_json_param};
    use crate::storage::{DbPool, Storage};

    /// In-memory SQLite with migrations applied.
    async fn test_db() -> DbPool {
        let storage = Storage::new("sqlite::memory:").await.unwrap();
        storage.run_migrations().await.unwrap();
        storage.pool().clone()
    }

    /// Seed a user + account; ids are plain TEXT on SQLite.
    async fn seed_account(db: &DbPool, user_id: &str, account_id: &str) {
        db_execute!(
            db,
            "INSERT INTO lyra_user (id, username, password_hash) VALUES (?, ?, ?)",
            &id_param(db, user_id).unwrap(),
            format!("user-{user_id}"),
            "hash"
        )
        .unwrap();
        db_execute!(
            db,
            r"
            INSERT INTO mail_account (
                id, user_id, email_address, protocol, auth_type, credential
            ) VALUES (?, ?, ?, 'imap', 'password', '{}')
            ",
            &id_param(db, account_id).unwrap(),
            &id_param(db, user_id).unwrap(),
            format!("{user_id}@example.com")
        )
        .unwrap();
    }

    async fn seed_folder(db: &DbPool, account_id: &str, folder_id: &str, role: Option<&str>) {
        db_execute!(
            db,
            "INSERT INTO folder (id, account_id, external_id, name, role) VALUES (?, ?, ?, ?, ?)",
            &id_param(db, folder_id).unwrap(),
            &id_param(db, account_id).unwrap(),
            folder_id,
            folder_id,
            role
        )
        .unwrap();
    }

    /// Seed one message `days_ago` days in the past.
    async fn seed_message(
        db: &DbPool,
        account_id: &str,
        folder_id: &str,
        external_id: &str,
        from_json: &str,
        days_ago: i64,
        is_read: bool,
    ) {
        let date = chrono::Utc::now() - chrono::Duration::days(days_ago);
        db_execute!(
            db,
            r"
            INSERT INTO message (id, account_id, folder_id, external_id, from_address, date, is_read)
            VALUES (?, ?, ?, ?, ?, ?, ?)
            ",
            &id_param(db, external_id).unwrap(),
            &id_param(db, account_id).unwrap(),
            &id_param(db, folder_id).unwrap(),
            external_id,
            opt_json_param(db, Some(from_json)),
            message_date_param(db, Some(&date.to_rfc3339())),
            is_read
        )
        .unwrap();
    }

    /// Seed the standard fixture: 3 days of inbound mail from 2 senders,
    /// sent-folder rows, and out-of-window rows. Returns today as YYYY-MM-DD.
    async fn seed_fixture(db: &DbPool) -> String {
        seed_account(db, "user-1", "acct-1").await;
        seed_folder(db, "acct-1", "fld-inbox", Some("inbox")).await;
        seed_folder(db, "acct-1", "fld-sent", Some("sent")).await;

        let alice = r#"{"name":"Alice","email":"alice@example.com"}"#;
        let bob = r#"{"name":"Bob","email":"bob@example.com"}"#;
        let me = r#"{"name":"Me","email":"me@example.com"}"#;

        // In-window inbound: today 3 (2 alice, 1 bob), 2d ago 1 alice, 5d ago 1 bob.
        seed_message(db, "acct-1", "fld-inbox", "m1", alice, 0, true).await;
        seed_message(db, "acct-1", "fld-inbox", "m2", alice, 0, false).await;
        seed_message(db, "acct-1", "fld-inbox", "m3", bob, 0, true).await;
        seed_message(db, "acct-1", "fld-inbox", "m4", alice, 2, false).await;
        seed_message(db, "acct-1", "fld-inbox", "m5", bob, 5, true).await;
        // In-window sent mail must not count as received.
        seed_message(db, "acct-1", "fld-sent", "s1", me, 1, true).await;
        seed_message(db, "acct-1", "fld-sent", "s2", me, 3, true).await;
        // Out-of-window rows (14-day window): one unread inbound, one sent.
        seed_message(db, "acct-1", "fld-inbox", "old1", alice, 40, false).await;
        seed_message(db, "acct-1", "fld-sent", "old2", me, 40, true).await;

        chrono::Utc::now().format("%Y-%m-%d").to_string()
    }

    #[test]
    fn clamp_days_defaults_and_bounds() {
        assert_eq!(clamp_days(None), 14);
        assert_eq!(clamp_days(Some(0)), 1);
        assert_eq!(clamp_days(Some(14)), 14);
        assert_eq!(clamp_days(Some(365)), 90);
    }

    #[tokio::test]
    async fn stats_aggregate_shape() {
        let db = test_db().await;
        let today = seed_fixture(&db).await;

        let stats = query_message_stats(&db, "user-1", 14).await.unwrap();

        assert_eq!(stats.days, 14);

        // Daily: 3 in-window days, ascending, excluding sent-folder rows.
        let daily: Vec<(&str, i64)> = stats
            .daily
            .iter()
            .map(|d| (d.date.as_str(), d.received))
            .collect();
        let two_days_ago = (chrono::Utc::now() - chrono::Duration::days(2))
            .format("%Y-%m-%d")
            .to_string();
        let five_days_ago = (chrono::Utc::now() - chrono::Duration::days(5))
            .format("%Y-%m-%d")
            .to_string();
        assert_eq!(
            daily,
            vec![
                (five_days_ago.as_str(), 1),
                (two_days_ago.as_str(), 1),
                (today.as_str(), 3),
            ]
        );

        // Top senders: alice (3) before bob (2), names decoded from JSON.
        assert_eq!(stats.top_senders.len(), 2);
        assert_eq!(stats.top_senders[0].address, "alice@example.com");
        assert_eq!(stats.top_senders[0].name.as_deref(), Some("Alice"));
        assert_eq!(stats.top_senders[0].count, 3);
        assert_eq!(stats.top_senders[1].address, "bob@example.com");
        assert_eq!(stats.top_senders[1].count, 2);

        // Totals: received/sent over the window; unread is an inbox snapshot.
        assert_eq!(stats.totals.received, 5);
        assert_eq!(stats.totals.sent, 2);
        assert_eq!(stats.totals.unread, 3); // m2, m4, and out-of-window old1
    }

    #[tokio::test]
    async fn stats_are_scoped_to_the_requesting_user() {
        // The single-user guard (migration 0005) blocks seeding a second user,
        // so verify scoping by querying as a user id that owns nothing.
        let db = test_db().await;
        seed_fixture(&db).await;

        let stats = query_message_stats(&db, "user-2", 14).await.unwrap();
        assert!(stats.daily.is_empty());
        assert!(stats.top_senders.is_empty());
        assert_eq!(stats.totals.received, 0);
        assert_eq!(stats.totals.sent, 0);
        assert_eq!(stats.totals.unread, 0);
    }

    #[tokio::test]
    async fn stats_empty_db_returns_zeros() {
        let db = test_db().await;
        seed_account(&db, "user-1", "acct-1").await;

        let stats = query_message_stats(&db, "user-1", 7).await.unwrap();
        assert_eq!(stats.days, 7);
        assert!(stats.daily.is_empty());
        assert!(stats.top_senders.is_empty());
        assert_eq!(stats.totals.received, 0);
        assert_eq!(stats.totals.sent, 0);
        assert_eq!(stats.totals.unread, 0);
    }
}
