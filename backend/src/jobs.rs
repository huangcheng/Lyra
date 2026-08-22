//! Durable SQL job queue and capped worker pool for mailbox sync.
//!
//! HTTP enqueues; workers `claim_due` and dispatch through `App`.
//! See `docs/specs/2026-08-22-lyra-plugin-kernel-spec.md` §5–6.

#![allow(clippy::doc_markdown)]
#![allow(clippy::missing_errors_doc)]

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sqlx::Row;
use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore};

use crate::kernel::App;
use crate::storage::DbPool;
use crate::sync::SyncError;

/// Job payload stored as JSON in `jobs.payload`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum JobPayload {
    SyncAccount {
        account_id: String,
        user_id: String,
    },
    UnsnoozeMessage {
        message_id: String,
    },
    SendMessage {
        account_id: String,
        outbound: serde_json::Value,
    },
}

/// A row claimed by a worker (`status = running`).
#[derive(Debug, Clone)]
pub struct ClaimedJob {
    pub id: String,
    #[allow(dead_code)]
    pub status: String,
    pub payload: JobPayload,
}

/// Per-account in-flight set so a second sync is skipped while one is running.
#[derive(Default)]
pub struct InFlight {
    accounts: Mutex<HashSet<String>>,
}

impl InFlight {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn try_begin(&self, account_id: &str) -> bool {
        self.accounts.lock().await.insert(account_id.to_string())
    }

    pub async fn finish(&self, account_id: &str) {
        self.accounts.lock().await.remove(account_id);
    }

    pub async fn contains(&self, account_id: &str) -> bool {
        self.accounts.lock().await.contains(account_id)
    }
}

fn payload_kind(payload: &JobPayload) -> &'static str {
    match payload {
        JobPayload::SyncAccount { .. } => "sync_account",
        JobPayload::UnsnoozeMessage { .. } => "unsnooze_message",
        JobPayload::SendMessage { .. } => "send_message",
    }
}

/// Persist a pending job. `run_at` is RFC3339 or `datetime('now')` text.
pub async fn enqueue(
    pool: &sqlx::SqlitePool,
    payload: &JobPayload,
    run_at: &str,
) -> Result<String, sqlx::Error> {
    let id = uuid::Uuid::now_v7().to_string();
    let kind = payload_kind(payload);
    let json = serde_json::to_string(payload).map_err(|e| sqlx::Error::Encode(Box::new(e)))?;
    sqlx::query(
        r"
        INSERT INTO jobs (id, kind, run_at, payload, status)
        VALUES (?, ?, ?, ?, 'pending')
        ",
    )
    .bind(&id)
    .bind(kind)
    .bind(run_at)
    .bind(&json)
    .execute(pool)
    .await?;
    Ok(id)
}

/// Atomically claim the oldest due pending job and mark it `running`.
#[allow(dead_code)]
pub async fn claim_due(
    pool: &sqlx::SqlitePool,
    now: &str,
) -> Result<Option<ClaimedJob>, sqlx::Error> {
    let row = sqlx::query(
        r"
        UPDATE jobs
        SET status = 'running', updated_at = datetime('now')
        WHERE id = (
            SELECT id FROM jobs
            WHERE status = 'pending' AND run_at <= ?
            ORDER BY run_at ASC, created_at ASC
            LIMIT 1
        )
        RETURNING id, payload, status
        ",
    )
    .bind(now)
    .fetch_optional(pool)
    .await?;

    row.as_ref().map(row_to_claimed).transpose()
}

/// Claim the next due job whose account is not already in-flight.
pub async fn claim_next(
    pool: &sqlx::SqlitePool,
    now: &str,
    inflight: &InFlight,
) -> Result<Option<ClaimedJob>, sqlx::Error> {
    let rows = sqlx::query(
        r"
        SELECT id, payload FROM jobs
        WHERE status = 'pending' AND run_at <= ?
        ORDER BY run_at ASC, created_at ASC
        ",
    )
    .bind(now)
    .fetch_all(pool)
    .await?;

    for row in rows {
        let payload_json: String = row.get("payload");
        let Ok(payload) = serde_json::from_str::<JobPayload>(&payload_json) else {
            continue;
        };
        if let JobPayload::SyncAccount { account_id, .. } = &payload
            && inflight.contains(account_id).await
        {
            continue;
        }
        let id: String = row.get("id");
        let updated = sqlx::query(
            r"
            UPDATE jobs
            SET status = 'running', updated_at = datetime('now')
            WHERE id = ? AND status = 'pending'
            ",
        )
        .bind(&id)
        .execute(pool)
        .await?;
        if updated.rows_affected() == 1 {
            return Ok(Some(ClaimedJob {
                id,
                status: "running".into(),
                payload,
            }));
        }
    }
    Ok(None)
}

/// Reset leftover `running` rows so a restarted worker can claim them.
pub async fn reclaim_stale_running(pool: &sqlx::SqlitePool) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        r"
        UPDATE jobs
        SET status = 'pending', updated_at = datetime('now')
        WHERE status = 'running'
        ",
    )
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// Acquire a worker permit, then claim. The permit is held for the job's
/// lifetime so `running` cannot exceed `SYNC_MAX_CONCURRENT`.
pub async fn try_claim_with_permit(
    pool: &sqlx::SqlitePool,
    now: &str,
    inflight: &InFlight,
    sem: &Arc<Semaphore>,
) -> Result<Option<(ClaimedJob, OwnedSemaphorePermit)>, sqlx::Error> {
    let Ok(permit) = Arc::clone(sem).try_acquire_owned() else {
        return Ok(None);
    };
    match claim_next(pool, now, inflight).await? {
        Some(job) => Ok(Some((job, permit))),
        None => Ok(None),
    }
}

#[allow(dead_code)]
fn row_to_claimed(row: &sqlx::sqlite::SqliteRow) -> Result<ClaimedJob, sqlx::Error> {
    let payload_json: String = row.get("payload");
    let payload =
        serde_json::from_str(&payload_json).map_err(|e| sqlx::Error::Decode(Box::new(e)))?;
    Ok(ClaimedJob {
        id: row.get("id"),
        status: row.get("status"),
        payload,
    })
}

/// Map a sync failure to a fixed, safe category string for `jobs.last_error`.
///
/// Whitelist, not blacklist: raw error text is never persisted, because it can
/// echo credentials, tokens, usernames, or server chatter.
fn sanitize_error(err: &SyncError) -> &'static str {
    match err {
        SyncError::Imap(_) => "IMAP error",
        SyncError::Jmap(_) => "JMAP error",
        SyncError::Smtp(_) => "SMTP error",
        SyncError::Database(_) => "database error",
        SyncError::Protocol(_) => "protocol error",
        _ => "sync failed",
    }
}

async fn mark_completed(pool: &sqlx::SqlitePool, id: &str) -> Result<(), sqlx::Error> {
    sqlx::query(
        r"
        UPDATE jobs
        SET status = 'completed', last_error = NULL, updated_at = datetime('now')
        WHERE id = ?
        ",
    )
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}

async fn mark_failed(
    pool: &sqlx::SqlitePool,
    id: &str,
    last_error: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r"
        UPDATE jobs
        SET status = 'failed', last_error = ?, attempts = attempts + 1,
            updated_at = datetime('now')
        WHERE id = ?
        ",
    )
    .bind(last_error)
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}

async fn revert_pending(pool: &sqlx::SqlitePool, id: &str) -> Result<(), sqlx::Error> {
    sqlx::query(
        r"
        UPDATE jobs
        SET status = 'pending', updated_at = datetime('now')
        WHERE id = ?
        ",
    )
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Dispatch a claimed job. Plugin errors mark the job `failed` (no panic).
/// `permit` must already be held; it is released when this future ends.
pub async fn process_job(
    pool: &sqlx::SqlitePool,
    app: &App,
    inflight: &InFlight,
    _permit: OwnedSemaphorePermit,
    job: ClaimedJob,
) -> Result<(), sqlx::Error> {
    match job.payload {
        JobPayload::SyncAccount {
            account_id,
            user_id,
        } => {
            if !inflight.try_begin(&account_id).await {
                revert_pending(pool, &job.id).await?;
                return Ok(());
            }
            let db = DbPool::Sqlite(pool.clone());
            let result = crate::sync::run_account_sync(&db, app, &user_id, &account_id).await;
            inflight.finish(&account_id).await;
            match result {
                Ok(_) => mark_completed(pool, &job.id).await?,
                Err(err) => {
                    let safe = sanitize_error(&err);
                    tracing::warn!(job_id = %job.id, error = %safe, "sync job failed");
                    mark_failed(pool, &job.id, safe).await?;
                }
            }
        }
        JobPayload::UnsnoozeMessage { message_id } => {
            sqlx::query("UPDATE message SET snoozed_until = NULL WHERE id = ?")
                .bind(&message_id)
                .execute(pool)
                .await?;
            mark_completed(pool, &job.id).await?;
        }
        JobPayload::SendMessage { .. } => {
            revert_pending(pool, &job.id).await?;
        }
    }
    Ok(())
}

/// Spawn the job poller after `AuthState` exists. Worker needs the db pool + `Arc<App>`.
pub fn spawn_workers(db: DbPool, app: Arc<App>, max_concurrent: usize) {
    match db {
        DbPool::Sqlite(pool) => spawn_sqlite_workers(pool, app, max_concurrent),
        #[cfg(feature = "postgres")]
        DbPool::Postgres(_) => {
            tracing::warn!("job workers are sqlite-only in this cycle");
        }
    }
}

fn spawn_sqlite_workers(pool: sqlx::SqlitePool, app: Arc<App>, max_concurrent: usize) {
    let inflight = Arc::new(InFlight::new());
    let sem = Arc::new(Semaphore::new(max_concurrent.max(1)));
    tokio::spawn(async move {
        if let Err(error) = reclaim_stale_running(&pool).await {
            tracing::error!(%error, "failed to reclaim stale running jobs");
        }
        loop {
            let now = chrono::Utc::now().to_rfc3339();
            match try_claim_with_permit(&pool, &now, &inflight, &sem).await {
                Ok(Some((job, permit))) => {
                    let pool = pool.clone();
                    let app = Arc::clone(&app);
                    let inflight = Arc::clone(&inflight);
                    tokio::spawn(async move {
                        if let Err(error) = process_job(&pool, &app, &inflight, permit, job).await {
                            tracing::error!(%error, "job process failed");
                        }
                    });
                }
                Ok(None) => {
                    tokio::time::sleep(Duration::from_millis(400)).await;
                }
                Err(error) => {
                    tracing::error!(%error, "job claim failed");
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::App;
    use crate::protocol::{ReceivePlugin, SyncCtx, SyncOutcome};
    use crate::storage::{DbPool, Storage};
    use crate::sync::SyncError;
    use async_trait::async_trait;
    use std::sync::Arc;
    use tokio::sync::Semaphore;

    async fn test_pool() -> sqlx::SqlitePool {
        let storage = Storage::new("sqlite::memory:").await.unwrap();
        storage.run_migrations().await.unwrap();
        match storage.pool().clone() {
            DbPool::Sqlite(pool) => pool,
            #[cfg(feature = "postgres")]
            DbPool::Postgres(_) => panic!("expected sqlite"),
        }
    }

    struct FailingReceive;

    #[async_trait]
    impl ReceivePlugin for FailingReceive {
        fn id(&self) -> &'static str {
            "imap"
        }

        async fn sync_account(&self, _ctx: &SyncCtx) -> Result<SyncOutcome, String> {
            Err("auth failed password=hunter2".into())
        }
    }

    async fn seed_account_with_cursor(pool: &sqlx::SqlitePool) -> (String, String, String) {
        let user_id = uuid::Uuid::now_v7().to_string();
        let account_id = uuid::Uuid::now_v7().to_string();
        let folder_id = uuid::Uuid::now_v7().to_string();
        let cursor_id = uuid::Uuid::now_v7().to_string();

        sqlx::query("INSERT INTO lyra_user (id, username, password_hash) VALUES (?, ?, ?)")
            .bind(&user_id)
            .bind("jobuser")
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
        .bind("Job Account")
        .bind("jobs@example.com")
        .bind("imap")
        .bind("password")
        .bind("{}")
        .bind("imap.example.com")
        .bind(993)
        .bind("tls")
        .execute(pool)
        .await
        .unwrap();

        sqlx::query(
            r"
            INSERT INTO folder (id, account_id, external_id, name, role)
            VALUES (?, ?, 'INBOX', 'INBOX', 'inbox')
            ",
        )
        .bind(&folder_id)
        .bind(&account_id)
        .execute(pool)
        .await
        .unwrap();

        sqlx::query(
            r"
            INSERT INTO sync_cursor (
                id, account_id, folder_id, protocol, cursor_type, cursor_value
            ) VALUES (?, ?, ?, 'imap', 'uidvalidity_uid', 'uid:100')
            ",
        )
        .bind(&cursor_id)
        .bind(&account_id)
        .bind(&folder_id)
        .execute(pool)
        .await
        .unwrap();

        (user_id, account_id, folder_id)
    }

    #[tokio::test]
    async fn enqueue_then_claim_due_job() {
        let pool = test_pool().await;
        let now = "2026-08-22T00:00:00+00:00";
        let payload = JobPayload::SyncAccount {
            account_id: "acc-1".into(),
            user_id: "user-1".into(),
        };

        let job_id = enqueue(&pool, &payload, now).await.unwrap();
        let claimed = claim_due(&pool, now)
            .await
            .unwrap()
            .expect("pending job should be claimed");

        assert_eq!(claimed.id, job_id);
        assert_eq!(claimed.status, "running");

        let status: String = sqlx::query_scalar("SELECT status FROM jobs WHERE id = ?")
            .bind(&job_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(status, "running");
    }

    #[tokio::test]
    async fn second_sync_same_account_skipped_while_running() {
        let pool = test_pool().await;
        let inflight = InFlight::new();
        let now = "2026-08-22T00:00:00+00:00";
        let payload = JobPayload::SyncAccount {
            account_id: "acc-1".into(),
            user_id: "user-1".into(),
        };

        let first_id = enqueue(&pool, &payload, now).await.unwrap();
        let first = claim_due(&pool, now).await.unwrap().expect("first job");
        assert_eq!(first.id, first_id);
        assert_eq!(first.status, "running");
        assert!(inflight.try_begin("acc-1").await);

        let second_id = enqueue(&pool, &payload, now).await.unwrap();
        let next = claim_next(&pool, now, &inflight).await.unwrap();
        assert!(
            next.is_none(),
            "second sync for the same account must be skipped while running"
        );

        let status: String = sqlx::query_scalar("SELECT status FROM jobs WHERE id = ?")
            .bind(&second_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(status, "pending");
    }

    #[tokio::test]
    async fn cursor_not_advanced_on_plugin_error() {
        let pool = test_pool().await;
        let (user_id, account_id, folder_id) = seed_account_with_cursor(&pool).await;
        let now = "2026-08-22T00:00:00+00:00";

        let mut app = App::new();
        app.register_receive(Arc::new(FailingReceive));

        let payload = JobPayload::SyncAccount {
            account_id: account_id.clone(),
            user_id,
        };
        let job_id = enqueue(&pool, &payload, now).await.unwrap();
        let claimed = claim_due(&pool, now).await.unwrap().expect("due job");

        let inflight = InFlight::new();
        let sem = Arc::new(Semaphore::new(3));
        let permit = Arc::clone(&sem)
            .try_acquire_owned()
            .expect("test semaphore has a permit");
        process_job(&pool, &app, &inflight, permit, claimed)
            .await
            .expect("plugin error must not panic");

        let row = sqlx::query_as::<_, (String, Option<String>)>(
            "SELECT status, last_error FROM jobs WHERE id = ?",
        )
        .bind(&job_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(row.0, "failed");
        let last_error = row.1.expect("last_error set");
        assert!(
            !last_error.to_lowercase().contains("password"),
            "last_error must not include passwords: {last_error}"
        );
        assert!(
            !last_error.contains("hunter2"),
            "last_error must not include secret values: {last_error}"
        );

        let cursor: String =
            sqlx::query_scalar("SELECT cursor_value FROM sync_cursor WHERE folder_id = ?")
                .bind(&folder_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(cursor, "uid:100");
    }

    #[tokio::test]
    async fn stale_running_job_reclaimed_then_claimable() {
        let pool = test_pool().await;
        let now = "2026-08-22T00:00:00+00:00";
        let inflight = InFlight::new();
        let payload = JobPayload::SyncAccount {
            account_id: "acc-stale".into(),
            user_id: "user-1".into(),
        };

        let job_id = enqueue(&pool, &payload, now).await.unwrap();
        sqlx::query("UPDATE jobs SET status = 'running' WHERE id = ?")
            .bind(&job_id)
            .execute(&pool)
            .await
            .unwrap();

        assert!(
            claim_next(&pool, now, &inflight).await.unwrap().is_none(),
            "running leftovers must not be claimed before reclaim"
        );

        let reclaimed = reclaim_stale_running(&pool).await.unwrap();
        assert_eq!(reclaimed, 1);

        let claimed = claim_next(&pool, now, &inflight)
            .await
            .unwrap()
            .expect("reclaimed job should be claimable via claim_next");
        assert_eq!(claimed.id, job_id);
        assert_eq!(claimed.status, "running");
    }

    #[tokio::test]
    async fn claim_does_not_exceed_semaphore_cap() {
        let pool = test_pool().await;
        let now = "2026-08-22T00:00:00+00:00";
        let inflight = InFlight::new();
        let sem = Arc::new(Semaphore::new(1));

        for i in 0..3 {
            enqueue(
                &pool,
                &JobPayload::SyncAccount {
                    account_id: format!("acc-{i}"),
                    user_id: "user-1".into(),
                },
                now,
            )
            .await
            .unwrap();
        }

        let first = try_claim_with_permit(&pool, now, &inflight, &sem)
            .await
            .unwrap()
            .expect("should claim when a permit is available");

        let second = try_claim_with_permit(&pool, now, &inflight, &sem)
            .await
            .unwrap();
        assert!(
            second.is_none(),
            "must not mark another job running without a permit"
        );

        let running: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM jobs WHERE status = 'running'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(running, 1, "running count must stay within the worker cap");
        drop(first);
    }

    #[test]
    fn sanitize_error_whitelists_categories() {
        // Raw text — tokens, usernames, server echoes — never passes through.
        let msg = "535 auth failed for user admin token=t0psecret";
        assert_eq!(
            sanitize_error(&SyncError::Imap(crate::imap::ImapError::Login(msg.into()))),
            "IMAP error"
        );
        assert_eq!(
            sanitize_error(&SyncError::Jmap(crate::jmap::JmapError::SessionDiscovery(
                msg.into()
            ))),
            "JMAP error"
        );
        assert_eq!(
            sanitize_error(&SyncError::Smtp(crate::smtp::SmtpError::Credential(
                msg.into()
            ))),
            "SMTP error"
        );
        assert_eq!(
            sanitize_error(&SyncError::Database(sqlx::Error::Protocol(msg.into()))),
            "database error"
        );
        assert_eq!(
            sanitize_error(&SyncError::Protocol(msg.into())),
            "protocol error"
        );
        // Other categories collapse to the generic fallback.
        assert_eq!(
            sanitize_error(&SyncError::InvalidInput(msg.into())),
            "sync failed"
        );
        for safe in [
            "IMAP error",
            "JMAP error",
            "SMTP error",
            "database error",
            "protocol error",
            "sync failed",
        ] {
            assert!(!safe.contains("t0psecret"));
            assert!(!safe.contains("admin"));
        }
    }
}
