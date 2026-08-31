//! Dashboard stats endpoint: read-only aggregates over local message rows.
//!
//! `GET /api/v1/messages/stats?days=14` feeds the analytics dashboard.
//! Additive and read-only: no sync state, no writes.

use axum::{
    Json, Router,
    extract::{Query, State},
    routing::get,
};
use sea_orm::sea_query::{Alias, Expr, ExprTrait, JoinType, Order, Query as Sq, SelectStatement};
use sea_orm::{ConnectionTrait, Value};
use serde::{Deserialize, Serialize};

use crate::auth::{AuthState, AuthUser};
use crate::db_row::{IdParam, id_param};
use crate::entities::{folder, mail_account, message};
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

/// Unwrap the driver error SeaORM wraps so `SyncError::Database` keeps
/// reporting the underlying `sqlx::Error`; non-driver SeaORM errors become
/// `sqlx::Error::Protocol` with the original message.
fn orm_err(err: sea_orm::DbErr) -> SyncError {
    use sea_orm::RuntimeErr;
    let sqlx_err = match err {
        sea_orm::DbErr::Exec(RuntimeErr::SqlxError(e))
        | sea_orm::DbErr::Query(RuntimeErr::SqlxError(e))
        | sea_orm::DbErr::Conn(RuntimeErr::SqlxError(e)) => std::sync::Arc::try_unwrap(e)
            .unwrap_or_else(|shared| sqlx::Error::Protocol(shared.to_string())),
        other => sqlx::Error::Protocol(other.to_string()),
    };
    SyncError::Database(sqlx_err)
}

/// Dialect-aware bind for a UUID-column value: TEXT on SQLite, native UUID on
/// Postgres — the same duality `IdParam` implements for the macro layer.
fn id_value(db: &DbPool, id: &str) -> Result<Value, SyncError> {
    Ok(match id_param(db, id)? {
        IdParam::Text(s) => Value::String(Some(s)),
        IdParam::Uuid(u) => Value::Uuid(Some(u)),
    })
}

/// Dialect-aware bind for the window cutoff: SQLite stores `message.date` as
/// `YYYY-MM-DD HH:MM:SS` text (lexicographic == chronological); Postgres
/// compares TIMESTAMPTZ natively.
fn cutoff_value(db: &DbPool, cutoff: chrono::DateTime<chrono::Utc>) -> Value {
    match db {
        DbPool::Sqlite(_) => Value::String(Some(cutoff.format("%Y-%m-%d %H:%M:%S").to_string())),
        #[cfg(feature = "postgres")]
        DbPool::Postgres(_) => Value::ChronoDateTimeUtc(Some(cutoff)),
    }
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
    let user = id_value(db, user_id)?;
    let cutoff = cutoff_value(db, chrono::Utc::now() - chrono::Duration::days(days));

    Ok(MessageStatsResponse {
        days,
        daily: query_daily(db, &user, &cutoff).await?,
        top_senders: query_top_senders(db, &user, &cutoff).await?,
        totals: query_totals(db, &user, &cutoff).await?,
    })
}

/// `message → folder → mail_account` joins shared by every aggregate.
fn add_message_joins(query: &mut SelectStatement) {
    query
        .from_as(message::Entity, Alias::new("m"))
        .join_as(
            JoinType::InnerJoin,
            folder::Entity,
            Alias::new("f"),
            Expr::cust("m.folder_id = f.id"),
        )
        .join_as(
            JoinType::InnerJoin,
            mail_account::Entity,
            Alias::new("a"),
            Expr::cust("m.account_id = a.id"),
        );
}

/// Window scope: owned by `user`, not soft-deleted, dated, within `cutoff`.
fn add_window_scope(query: &mut SelectStatement, user: &Value, cutoff: &Value) {
    add_message_joins(query);
    query
        .and_where(Expr::cust("a.user_id").eq(Expr::val(user.clone())))
        .and_where(Expr::cust("m.is_deleted").eq(Expr::val(false)))
        .and_where(Expr::cust("m.date IS NOT NULL"))
        .and_where(Expr::cust("m.date").gte(Expr::val(cutoff.clone())));
}

/// Per-day received counts over the window, ascending by date.
///
/// CAST to TEXT keeps the date decode identical on both engines
/// (SQLite `date()` → TEXT; Postgres `date()` → DATE → text). Bucketing is
/// UTC on both: SQLite dates are UTC text, and Postgres sessions are pinned
/// to `TIME ZONE UTC` at connect (see `storage.rs`).
async fn query_daily(
    db: &DbPool,
    user: &Value,
    cutoff: &Value,
) -> Result<Vec<DailyCount>, SyncError> {
    let mut query = Sq::select();
    query
        .expr_as(Expr::cust("CAST(date(m.date) AS TEXT)"), Alias::new("d"))
        .expr_as(Expr::cust("COUNT(*)"), Alias::new("c"));
    add_window_scope(&mut query, user, cutoff);
    query
        .and_where(Expr::cust(
            "(f.role IS NULL OR f.role NOT IN ('sent', 'drafts'))",
        ))
        .add_group_by([Expr::cust("CAST(date(m.date) AS TEXT)")])
        .order_by_expr(Expr::cust("d"), Order::Asc);

    let rows = db.orm().query_all(&query).await.map_err(orm_err)?;
    rows.iter()
        .map(|row| {
            Ok(DailyCount {
                date: row.try_get("", "d").map_err(orm_err)?,
                received: row.try_get("", "c").map_err(orm_err)?,
            })
        })
        .collect()
}

/// Top 5 senders by received count over the window.
///
/// Groups by the raw JSON address object (TEXT on SQLite, JSONB on
/// Postgres — both decode as `serde_json::Value` through the entity layer);
/// name/email extraction happens in Rust.
async fn query_top_senders(
    db: &DbPool,
    user: &Value,
    cutoff: &Value,
) -> Result<Vec<SenderCount>, SyncError> {
    // Group by the sender *email* inside the From JSON, not the whole JSON
    // blob — the same mailbox with different display-name encodings must be
    // one sender. JSON accessors are the one genuine dialect branch here
    // (SQLite stores JSON as TEXT, Postgres as JSONB).
    let (email_expr, name_expr) = match db {
        DbPool::Sqlite(_) => (
            "json_extract(m.from_address, '$.email')",
            "json_extract(m.from_address, '$.name')",
        ),
        #[cfg(feature = "postgres")]
        DbPool::Postgres(_) => ("(m.from_address->>'email')", "(m.from_address->>'name')"),
    };
    let mut query = Sq::select();
    query
        .expr_as(Expr::cust(email_expr), Alias::new("sender_email"))
        .expr_as(
            Expr::cust(format!("MAX({name_expr})")),
            Alias::new("sender_name"),
        )
        .expr_as(Expr::cust("COUNT(*)"), Alias::new("c"));
    add_window_scope(&mut query, user, cutoff);
    query
        .and_where(Expr::cust(
            "(f.role IS NULL OR f.role NOT IN ('sent', 'drafts'))",
        ))
        .and_where(Expr::cust("m.from_address IS NOT NULL"))
        .add_group_by([Expr::cust(email_expr)])
        .order_by_expr(Expr::cust("c"), Order::Desc)
        .limit(5);

    let rows = db.orm().query_all(&query).await.map_err(orm_err)?;
    let rows = rows
        .iter()
        .map(|row| {
            let address: Option<String> = row.try_get("", "sender_email").map_err(orm_err)?;
            let name: Option<String> = row.try_get("", "sender_name").map_err(orm_err)?;
            let count: i64 = row.try_get("", "c").map_err(orm_err)?;
            Ok((address, name, count))
        })
        .collect::<Result<Vec<_>, SyncError>>()?;

    Ok(rows
        .into_iter()
        .filter_map(|(address, name, count)| {
            let address = address.filter(|a| !a.trim().is_empty())?;
            Some(SenderCount {
                address,
                name,
                count,
            })
        })
        .collect())
}

/// `SELECT COUNT(*)` with the caller's scope applied.
async fn count_rows(
    db: &DbPool,
    build: impl FnOnce(&mut SelectStatement),
) -> Result<i64, SyncError> {
    let mut query = Sq::select();
    query.expr_as(Expr::cust("COUNT(*)"), Alias::new("c"));
    build(&mut query);
    let row = db
        .orm()
        .query_one(&query)
        .await
        .map_err(orm_err)?
        .ok_or_else(|| SyncError::Internal("count query returned no rows".into()))?;
    row.try_get::<i64>("", "c").map_err(orm_err)
}

/// Window totals for received/sent plus the current unread inbox snapshot.
async fn query_totals(db: &DbPool, user: &Value, cutoff: &Value) -> Result<StatsTotals, SyncError> {
    let received = count_rows(db, |query| {
        add_window_scope(query, user, cutoff);
        query.and_where(Expr::cust(
            "(f.role IS NULL OR f.role NOT IN ('sent', 'drafts'))",
        ));
    })
    .await?;

    let sent = count_rows(db, |query| {
        add_window_scope(query, user, cutoff);
        query.and_where(Expr::cust("f.role = 'sent'"));
    })
    .await?;

    // Unread snapshot: inbox role, read flag unset, not deleted, not snoozed —
    // the same visibility filters the /messages list handlers apply. The
    // "now" comparison is the one genuine dialect branch: SQLite writes
    // `datetime('now')` text, Postgres uses NOW().
    let now_expr = match db {
        DbPool::Sqlite(_) => "datetime('now')",
        #[cfg(feature = "postgres")]
        DbPool::Postgres(_) => "NOW()",
    };
    let unread = count_rows(db, |query| {
        add_message_joins(query);
        let snooze_cmp = format!("(m.snoozed_until IS NULL OR m.snoozed_until <= {now_expr})");
        query
            .and_where(Expr::cust("a.user_id").eq(Expr::val(user.clone())))
            .and_where(Expr::cust("f.role = 'inbox'"))
            .and_where(Expr::cust("m.is_read").eq(Expr::val(false)))
            .and_where(Expr::cust("m.is_deleted").eq(Expr::val(false)))
            .and_where(Expr::cust(snooze_cmp));
    })
    .await?;

    Ok(StatsTotals {
        received,
        sent,
        unread,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{DbPool, Storage};

    /// In-memory SQLite with migrations applied.
    async fn test_db() -> DbPool {
        let storage = Storage::new("sqlite::memory:").await.unwrap();
        storage.run_migrations().await.unwrap();
        storage.pool().clone()
    }

    /// Dialect-aware bind for fixture ids (TEXT on SQLite / UUID on PG).
    fn bind_id(db: &DbPool, id: &str) -> sea_orm::Value {
        match crate::db_row::id_param(db, id).unwrap() {
            crate::db_row::IdParam::Text(s) => sea_orm::Value::String(Some(s)),
            crate::db_row::IdParam::Uuid(u) => sea_orm::Value::Uuid(Some(u)),
        }
    }

    async fn exec(
        conn: &sea_orm::DatabaseConnection,
        db: &DbPool,
        sql: &str,
        values: Vec<sea_orm::Value>,
    ) {
        let stmt = sea_orm::Statement::from_sql_and_values(db.backend(), sql, values);
        conn.query_one_raw(stmt).await.unwrap();
    }

    /// Seed a user + account; ids are plain TEXT on SQLite.
    async fn seed_account(db: &DbPool, user_id: &str, account_id: &str) {
        let conn = db.orm();
        exec(
            &conn,
            db,
            "INSERT INTO lyra_user (id, username, password_hash) VALUES (?, ?, ?)",
            vec![
                bind_id(db, user_id),
                format!("user-{user_id}").into(),
                "hash".into(),
            ],
        )
        .await;
        exec(
            &conn,
            db,
            r"
            INSERT INTO mail_account (
                id, user_id, email_address, protocol, auth_type, credential
            ) VALUES (?, ?, ?, 'imap', 'password', '{}')
            ",
            vec![
                bind_id(db, account_id),
                bind_id(db, user_id),
                format!("{user_id}@example.com").into(),
            ],
        )
        .await;
    }

    async fn seed_folder(db: &DbPool, account_id: &str, folder_id: &str, role: Option<&str>) {
        let conn = db.orm();
        exec(
            &conn,
            db,
            "INSERT INTO folder (id, account_id, external_id, name, role) VALUES (?, ?, ?, ?, ?)",
            vec![
                bind_id(db, folder_id),
                bind_id(db, account_id),
                folder_id.into(),
                folder_id.into(),
                role.map(str::to_string).into(),
            ],
        )
        .await;
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
        let conn = db.orm();
        exec(
            &conn,
            db,
            r"
            INSERT INTO message (id, account_id, folder_id, external_id, from_address, date, is_read)
            VALUES (?, ?, ?, ?, ?, ?, ?)
            ",
            vec![
                bind_id(db, external_id),
                bind_id(db, account_id),
                bind_id(db, folder_id),
                external_id.into(),
                from_json.into(),
                // Same UTC text the macro layer's message_date_param wrote.
                date.format("%Y-%m-%d %H:%M:%S").to_string().into(),
                is_read.into(),
            ],
        )
        .await;
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
