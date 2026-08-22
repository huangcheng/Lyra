//! Background poll scheduler: startup tick + interval sync enqueue.
//!
//! See `docs/specs/2026-08-22-lyra-plugin-kernel-spec.md` §4 triggers.

#![allow(clippy::doc_markdown)]
#![allow(clippy::missing_errors_doc)]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use sqlx::Row;
use tokio::sync::Mutex;

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
    db_fetch_all!(
        db,
        r"
        SELECT id, user_id FROM mail_account
        WHERE is_active = 1 AND sync_enabled = 1
        ",
        |row| ActiveAccount {
            id: row.get("id"),
            user_id: row.get("user_id"),
        }
    )
}

/// Id of a pending/running `sync_account` job for this account, if any.
pub async fn pending_or_running_sync_job_id(
    db: &DbPool,
    account_id: &str,
) -> Result<Option<String>, sqlx::Error> {
    let rows = db_fetch_all!(
        db,
        r"
        SELECT id, payload FROM jobs
        WHERE kind = 'sync_account' AND status IN ('pending', 'running')
        ",
        |row| {
            let id: String = row.get("id");
            let payload_json: String = row.get("payload");
            (id, payload_json)
        }
    )?;

    for (id, payload_json) in rows {
        let Ok(payload) = serde_json::from_str::<JobPayload>(&payload_json) else {
            continue;
        };
        if let JobPayload::SyncAccount {
            account_id: aid, ..
        } = payload
            && aid == account_id
        {
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
async fn latest_terminal_sync(
    db: &DbPool,
    account_id: &str,
) -> Result<Option<(String, String)>, sqlx::Error> {
    let rows = db_fetch_all!(
        db,
        r"
        SELECT id, payload, status FROM jobs
        WHERE kind = 'sync_account' AND status IN ('completed', 'failed')
        ORDER BY updated_at DESC, created_at DESC
        ",
        |row| {
            let id: String = row.get("id");
            let payload_json: String = row.get("payload");
            let status: String = row.get("status");
            (id, payload_json, status)
        }
    )?;

    for (job_id, payload_json, status) in rows {
        let Ok(payload) = serde_json::from_str::<JobPayload>(&payload_json) else {
            continue;
        };
        if let JobPayload::SyncAccount {
            account_id: aid, ..
        } = payload
            && aid == account_id
        {
            return Ok(Some((job_id, status)));
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
        assert_eq!(b.delay("a"), Duration::from_secs(60));
        b.fail("a");
        assert_eq!(b.delay("a"), Duration::from_secs(120));
        b.ok("a");
        assert_eq!(b.delay("a"), Duration::from_secs(60));
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
