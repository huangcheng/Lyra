//! Background poll scheduler: startup tick + interval sync enqueue.
//!
//! See `docs/specs/2026-08-22-lyra-plugin-kernel-spec.md` §4 triggers.

#![allow(clippy::doc_markdown)]
#![allow(clippy::missing_errors_doc)]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use sea_orm::sea_query::Query;
use sea_orm::{ColumnTrait, ConnectionTrait};
use tokio::sync::Mutex;

use crate::entities::{jobs, mail_account};
use crate::jobs::{JobPayload, enqueue};
use crate::storage::DbPool;

const BASE_POLL_SECS: u64 = 300;
const MAX_BACKOFF_SECS: u64 = 3600;

/// Per-account poll delay. Doubles on failure up to 1 hour; resets on success.
#[derive(Debug)]
pub struct Backoff {
    delays: HashMap<String, Duration>,
    next_due: HashMap<String, Instant>,
    /// Job id whose terminal status was already applied (avoid re-doubling).
    applied_job: HashMap<String, String>,
    base: Duration,
    max: Duration,
}

impl Default for Backoff {
    fn default() -> Self {
        Self::with_base_secs(BASE_POLL_SECS)
    }
}

impl Backoff {
    /// Build a backoff whose base interval matches `SYNC_POLL_SECS`.
    #[must_use]
    pub fn with_base_secs(base_secs: u64) -> Self {
        Self {
            delays: HashMap::new(),
            next_due: HashMap::new(),
            applied_job: HashMap::new(),
            base: Duration::from_secs(base_secs.max(1)),
            max: Duration::from_secs(MAX_BACKOFF_SECS),
        }
    }

    /// Current poll delay for `account_id` (base interval if never failed).
    #[must_use]
    pub fn delay(&self, account_id: &str) -> Duration {
        self.delays.get(account_id).copied().unwrap_or(self.base)
    }

    /// Record a sync failure: double delay up to the cap.
    pub fn fail(&mut self, account_id: &str) {
        let next = self.delay(account_id).saturating_mul(2).min(self.max);
        self.delays.insert(account_id.to_string(), next);
    }

    /// Record a sync success: reset delay to the base interval.
    pub fn ok(&mut self, account_id: &str) {
        self.delays.remove(account_id);
    }

    /// Apply a terminal job outcome once per job id.
    fn observe_outcome(&mut self, account_id: &str, job_id: &str, status: &str) {
        if self
            .applied_job
            .get(account_id)
            .is_some_and(|id| id == job_id)
        {
            return;
        }
        self.applied_job
            .insert(account_id.to_string(), job_id.to_string());
        match status {
            "failed" => {
                self.fail(account_id);
                // Stretch the next poll using the new (doubled) delay.
                let d = self.delay(account_id);
                self.next_due
                    .insert(account_id.to_string(), Instant::now() + d);
            }
            "completed" => {
                // Reset delay only; keep existing next_due from mark_enqueued.
                self.ok(account_id);
            }
            _ => {}
        }
    }

    fn is_due(&self, account_id: &str, now: Instant) -> bool {
        match self.next_due.get(account_id) {
            None => true,
            Some(due) => now >= *due,
        }
    }

    fn mark_enqueued(&mut self, account_id: &str) {
        let d = self.delay(account_id);
        self.next_due
            .insert(account_id.to_string(), Instant::now() + d);
    }
}

struct ActiveAccount {
    id: String,
    user_id: String,
}

/// Unwrap the driver error SeaORM wraps so this module keeps reporting
/// `sqlx::Error` (SeaORM errors that are not driver errors become
/// `sqlx::Error::Protocol` with the original message).
fn orm_err(err: sea_orm::DbErr) -> sqlx::Error {
    use sea_orm::RuntimeErr;
    match err {
        sea_orm::DbErr::Exec(RuntimeErr::SqlxError(e))
        | sea_orm::DbErr::Query(RuntimeErr::SqlxError(e))
        | sea_orm::DbErr::Conn(RuntimeErr::SqlxError(e)) => std::sync::Arc::try_unwrap(e)
            .unwrap_or_else(|shared| sqlx::Error::Protocol(shared.to_string())),
        other => sqlx::Error::Protocol(other.to_string()),
    }
}

/// Decode a UUID/TEXT id column: `String` on SQLite, native UUID on Postgres.
fn row_id(row: &sea_orm::QueryResult, col: &str) -> Result<String, sqlx::Error> {
    if let Some(s) = row.try_get::<Option<String>>("", col).ok().flatten() {
        return Ok(s);
    }
    row.try_get::<Option<uuid::Uuid>>("", col)
        .map_err(orm_err)?
        .map(|u| u.to_string())
        .ok_or_else(|| sqlx::Error::Protocol(format!("missing id column {col}")))
}

/// One poll pass: enqueue `SyncAccount` for due active accounts not already queued.
///
/// Returns how many jobs were enqueued.
pub async fn poll_tick(db: &DbPool, backoff: &mut Backoff) -> Result<usize, sqlx::Error> {
    let accounts = list_active_accounts(db).await?;
    let now = Instant::now();
    let run_at = chrono::Utc::now().to_rfc3339();
    let mut enqueued = 0usize;

    for account in accounts {
        if let Some((job_id, status)) = latest_terminal_sync(db, &account.id).await? {
            backoff.observe_outcome(&account.id, &job_id, &status);
        }
        if !backoff.is_due(&account.id, now) {
            continue;
        }
        if has_pending_or_running_sync(db, &account.id).await? {
            continue;
        }
        enqueue(
            db,
            &JobPayload::SyncAccount {
                account_id: account.id.clone(),
                user_id: account.user_id,
            },
            &run_at,
        )
        .await?;
        backoff.mark_enqueued(&account.id);
        enqueued += 1;
    }

    Ok(enqueued)
}

async fn list_active_accounts(db: &DbPool) -> Result<Vec<ActiveAccount>, sqlx::Error> {
    let mut query = Query::select();
    query
        .column(mail_account::Column::Id)
        .column(mail_account::Column::UserId)
        .from(mail_account::Entity)
        .and_where(mail_account::Column::IsActive.eq(true))
        .and_where(mail_account::Column::SyncEnabled.eq(true));

    let rows = db.orm().query_all(&query).await.map_err(orm_err)?;
    let mut accounts = Vec::with_capacity(rows.len());
    for row in &rows {
        accounts.push(ActiveAccount {
            id: row_id(row, "id")?,
            user_id: row_id(row, "user_id")?,
        });
    }
    Ok(accounts)
}

/// Id of a pending/running `sync_account` job for this account, if any.
pub async fn pending_or_running_sync_job_id(
    db: &DbPool,
    account_id: &str,
) -> Result<Option<String>, sqlx::Error> {
    // Column-select with text decoding: the jobs table keeps TEXT timestamp
    // columns on Postgres (0004), which the typed entity cannot decode there.
    let mut stmt = sea_orm::sea_query::Query::select();
    stmt.columns([jobs::Column::Id, jobs::Column::Payload])
        .from(jobs::Entity)
        .and_where(jobs::Column::Kind.eq("sync_account"))
        .and_where(jobs::Column::Status.is_in(["pending", "running"]))
        .order_by(jobs::Column::RunAt, sea_orm::sea_query::Order::Asc)
        .order_by(jobs::Column::CreatedAt, sea_orm::sea_query::Order::Asc);
    let rows = db.orm().query_all(&stmt).await.map_err(orm_err)?;

    for row in rows {
        let Ok(payload_json) = row.try_get::<String>("", "payload") else {
            continue;
        };
        let Ok(payload) = serde_json::from_str::<JobPayload>(&payload_json) else {
            continue;
        };
        if let JobPayload::SyncAccount {
            account_id: aid, ..
        } = payload
            && aid == account_id
        {
            let id = row.try_get::<String>("", "id").map_err(orm_err)?;
            return Ok(Some(id));
        }
    }
    Ok(None)
}

/// True when a sync job for this account is already pending or running.
pub async fn has_pending_or_running_sync(
    db: &DbPool,
    account_id: &str,
) -> Result<bool, sqlx::Error> {
    Ok(pending_or_running_sync_job_id(db, account_id)
        .await?
        .is_some())
}

/// Most recent completed/failed sync job for an account, if any.
pub(crate) async fn latest_terminal_sync(
    db: &DbPool,
    account_id: &str,
) -> Result<Option<(String, String)>, sqlx::Error> {
    // Column-select with text decoding (see pending_or_running_sync_job_id).
    let mut stmt = sea_orm::sea_query::Query::select();
    stmt.columns([
        jobs::Column::Id,
        jobs::Column::Payload,
        jobs::Column::Status,
    ])
    .from(jobs::Entity)
    .and_where(jobs::Column::Kind.eq("sync_account"))
    .and_where(jobs::Column::Status.is_in(["completed", "failed"]))
    .order_by(jobs::Column::UpdatedAt, sea_orm::sea_query::Order::Desc)
    .order_by(jobs::Column::CreatedAt, sea_orm::sea_query::Order::Desc);
    let rows = db.orm().query_all(&stmt).await.map_err(orm_err)?;

    for row in rows {
        let Ok(payload_json) = row.try_get::<String>("", "payload") else {
            continue;
        };
        let Ok(payload) = serde_json::from_str::<JobPayload>(&payload_json) else {
            continue;
        };
        if let JobPayload::SyncAccount {
            account_id: aid, ..
        } = payload
            && aid == account_id
        {
            let id = row.try_get::<String>("", "id").map_err(orm_err)?;
            let status = row.try_get::<String>("", "status").map_err(orm_err)?;
            return Ok(Some((id, status)));
        }
    }
    Ok(None)
}

/// Start the background poll loop. First tick runs immediately (no initial wait).
pub fn start_scheduler(db: DbPool, poll_secs: u64) {
    let secs = poll_secs.max(1);
    tokio::spawn(async move {
        let backoff = Arc::new(Mutex::new(Backoff::with_base_secs(secs)));
        let mut interval = tokio::time::interval(Duration::from_secs(secs));
        // First tick completes immediately — do not wait poll_secs at startup.
        loop {
            interval.tick().await;
            let mut guard = backoff.lock().await;
            match poll_tick(&db, &mut guard).await {
                Ok(n) if n > 0 => {
                    tracing::debug!(enqueued = n, "scheduler poll enqueued sync jobs");
                }
                Ok(_) => {}
                Err(error) => {
                    tracing::error!(%error, "scheduler poll failed");
                }
            }
            match crate::ics::refresh_due_subscriptions(&db).await {
                Ok(n) if n > 0 => {
                    tracing::debug!(refreshed = n, "ICS subscriptions refreshed");
                }
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(%error, "ICS subscription refresh pass failed");
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jobs::{JobPayload, enqueue};
    use crate::storage::{DbPool, Storage};
    use std::time::Duration;

    async fn test_pool() -> DbPool {
        let storage = Storage::new("sqlite::memory:").await.unwrap();
        storage.run_migrations().await.unwrap();
        storage.pool().clone()
    }

    fn sqlite_pool(db: &DbPool) -> &sqlx::SqlitePool {
        match db {
            DbPool::Sqlite(pool) => pool,
            #[cfg(feature = "postgres")]
            DbPool::Postgres(_) => panic!("expected sqlite"),
        }
    }

    async fn seed_active_account(db: &DbPool) -> (String, String) {
        let pool = sqlite_pool(db);
        let user_id = uuid::Uuid::now_v7().to_string();
        let account_id = uuid::Uuid::now_v7().to_string();

        sqlx::query("INSERT INTO lyra_user (id, username, password_hash) VALUES (?, ?, ?)")
            .bind(&user_id)
            .bind("scheduser")
            .bind("hash")
            .execute(pool)
            .await
            .unwrap();

        sqlx::query(
            r"
            INSERT INTO mail_account (
                id, user_id, display_name, email_address, protocol, auth_type,
                credential, imap_host, imap_port, imap_security,
                is_active, sync_enabled
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 1, 1)
            ",
        )
        .bind(&account_id)
        .bind(&user_id)
        .bind("Sched Account")
        .bind("sched@example.com")
        .bind("imap")
        .bind("password")
        .bind("{}")
        .bind("imap.example.com")
        .bind(993)
        .bind("tls")
        .execute(pool)
        .await
        .unwrap();

        (user_id, account_id)
    }

    #[tokio::test]
    async fn poll_skips_account_already_in_flight() {
        let pool = test_pool().await;
        let (user_id, account_id) = seed_active_account(&pool).await;
        let now = chrono::Utc::now().to_rfc3339();

        enqueue(
            &pool,
            &JobPayload::SyncAccount {
                account_id: account_id.clone(),
                user_id: user_id.clone(),
            },
            &now,
        )
        .await
        .unwrap();

        let mut backoff = Backoff::default();
        let enqueued = poll_tick(&pool, &mut backoff).await.unwrap();
        assert_eq!(
            enqueued, 0,
            "must not enqueue another sync while one is pending/running"
        );

        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM jobs WHERE kind = 'sync_account' AND status IN ('pending', 'running')",
        )
        .fetch_one(sqlite_pool(&pool))
        .await
        .unwrap();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    #[allow(clippy::duration_suboptimal_units)] // brief specifies from_secs(300/600)
    async fn backoff_doubles_after_failures() {
        let mut b = Backoff::default();
        assert_eq!(b.delay("a"), Duration::from_secs(300));
        b.fail("a");
        assert_eq!(b.delay("a"), Duration::from_secs(600));
        b.ok("a");
        assert_eq!(b.delay("a"), Duration::from_secs(300));
    }

    #[test]
    fn backoff_base_follows_configured_poll_secs() {
        let mut b = Backoff::with_base_secs(60);
        assert_eq!(b.delay("a"), Duration::from_mins(1));
        b.fail("a");
        assert_eq!(b.delay("a"), Duration::from_mins(2));
        b.ok("a");
        assert_eq!(b.delay("a"), Duration::from_mins(1));
    }

    #[tokio::test]
    async fn poll_enqueues_active_accounts() {
        let pool = test_pool().await;
        let (user_id, account_id) = seed_active_account(&pool).await;

        let mut backoff = Backoff::default();
        let enqueued = poll_tick(&pool, &mut backoff).await.unwrap();
        assert_eq!(enqueued, 1);

        let row = sqlx::query_as::<_, (String, String)>(
            "SELECT payload, status FROM jobs WHERE kind = 'sync_account'",
        )
        .fetch_one(sqlite_pool(&pool))
        .await
        .unwrap();
        assert_eq!(row.1, "pending");
        let payload: JobPayload = serde_json::from_str(&row.0).unwrap();
        match payload {
            JobPayload::SyncAccount {
                account_id: aid,
                user_id: uid,
            } => {
                assert_eq!(aid, account_id);
                assert_eq!(uid, user_id);
            }
            other => panic!("unexpected payload: {other:?}"),
        }
    }
}
