//! Durable SQL job queue and capped worker pool for mailbox sync.
//!
//! HTTP enqueues; workers `claim_due` and dispatch through `App`.
//! See `docs/specs/2026-08-22-lyra-plugin-kernel-spec.md` §5–6.

#![allow(clippy::doc_markdown)]
#![allow(clippy::missing_errors_doc)]

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use sea_orm::sea_query::{Expr, Order, Query as Sq};
use sea_orm::{
    ColumnTrait, ConnectionTrait, DbBackend, EntityTrait, QueryFilter, Statement, Value,
};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore};

use crate::entities::{jobs, mail_account, message};
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

// ── SeaORM plumbing ──────────────────────────────────────────────────
//
// `jobs` ids are TEXT in both dialects (UUIDv7 strings, not native UUID
// columns), so no Uuid/TEXT split applies here. Timestamp columns are also
// TEXT on both engines — the Postgres default even casts NOW() to ::TEXT —
// so stamps bind as UTC text rather than an engine-side expression, which
// keeps the assignment type valid on Postgres.

/// Unwrap the driver error SeaORM wraps so callers keep a `sqlx::Error`;
/// non-driver SeaORM errors become `sqlx::Error::Protocol`.
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

/// Decode a `RETURNING id, payload, status` row into a claimed job. Payload
/// JSON that fails to parse surfaces as `sqlx::Error::Decode`, exactly like
/// the macro-layer row mapping did.
fn claimed_from_row(row: &sea_orm::QueryResult) -> Result<ClaimedJob, sqlx::Error> {
    let id: String = row.try_get("", "id").map_err(orm_err)?;
    let status: String = row.try_get("", "status").map_err(orm_err)?;
    let payload_json: String = row.try_get("", "payload").map_err(orm_err)?;
    let payload =
        serde_json::from_str(&payload_json).map_err(|e| sqlx::Error::Decode(Box::new(e)))?;
    Ok(ClaimedJob {
        id,
        status,
        payload,
    })
}

/// TEXT stamp shaped like the legacy writers: `datetime('now')`-style second
/// precision on SQLite; the migration-default `(NOW() AT TIME ZONE 'UTC')
/// ::TEXT` shape (micro precision) on Postgres.
fn updated_at_value(db: &DbPool) -> Value {
    let fmt = match db {
        DbPool::Sqlite(_) => "%Y-%m-%d %H:%M:%S",
        #[cfg(feature = "postgres")]
        DbPool::Postgres(_) => "%Y-%m-%d %H:%M:%S%.6f",
    };
    Value::String(Some(chrono::Utc::now().format(fmt).to_string()))
}

/// Dialect-aware bind for UUID-typed foreign-key ids (`message`, `mail_account`).
fn fk_id_value(db: &DbPool, id: &str) -> Result<Value, sqlx::Error> {
    use crate::db_row::{IdParam, id_param};
    Ok(match id_param(db, id).map_err(sqlx::Error::from)? {
        IdParam::Text(s) => Value::String(Some(s)),
        IdParam::Uuid(u) => Value::Uuid(Some(u)),
    })
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
    db: &DbPool,
    payload: &JobPayload,
    run_at: &str,
) -> Result<String, sqlx::Error> {
    let id = uuid::Uuid::now_v7().to_string();
    let kind = payload_kind(payload);
    let json = serde_json::to_string(payload).map_err(|e| sqlx::Error::Encode(Box::new(e)))?;
    let mut insert = Sq::insert();
    insert
        .into_table(jobs::Entity)
        .columns([
            jobs::Column::Id,
            jobs::Column::Kind,
            jobs::Column::RunAt,
            jobs::Column::Payload,
            jobs::Column::Status,
        ])
        .values_panic([
            Expr::val(id.as_str()),
            Expr::val(kind),
            Expr::val(run_at),
            Expr::val(json.as_str()),
            Expr::val("pending"),
        ]);
    db.orm().execute(&insert).await.map_err(orm_err)?;
    Ok(id)
}

/// SQL for [`claim_due`] per engine. The updated stamp is bound (not a
/// literal) because the column is TEXT on both engines; only placeholders
/// differ.
const CLAIM_DUE_SQL_SQLITE: &str = r"
        UPDATE jobs
        SET status = 'running', updated_at = ?
        WHERE id = (
            SELECT id FROM jobs
            WHERE status = 'pending' AND run_at <= ?
            ORDER BY run_at ASC, created_at ASC
            LIMIT 1
        )
        RETURNING id, payload, status
        ";

#[cfg(feature = "postgres")]
const CLAIM_DUE_SQL_POSTGRES: &str = r"
        UPDATE jobs
        SET status = 'running', updated_at = $2
        WHERE id = (
            SELECT id FROM jobs
            WHERE status = 'pending' AND run_at <= $1
            ORDER BY run_at ASC, created_at ASC
            LIMIT 1
        )
        RETURNING id, payload, status
        ";

/// Atomically claim the oldest due pending job and mark it `running`.
///
/// Single-statement UPDATE…RETURNING (valid on both engines), executed as a
/// raw parameterized statement on the SeaORM connection.
#[allow(dead_code)]
pub async fn claim_due(db: &DbPool, now: &str) -> Result<Option<ClaimedJob>, sqlx::Error> {
    let stmt = match db.backend() {
        DbBackend::Sqlite => Statement::from_sql_and_values(
            DbBackend::Sqlite,
            CLAIM_DUE_SQL_SQLITE,
            [updated_at_value(db), Value::from(now)],
        ),
        #[cfg(feature = "postgres")]
        DbBackend::Postgres => Statement::from_sql_and_values(
            DbBackend::Postgres,
            CLAIM_DUE_SQL_POSTGRES,
            [Value::from(now), updated_at_value(db)],
        ),
        other => {
            return Err(sqlx::Error::Protocol(format!(
                "jobs: unsupported backend {other:?}"
            )));
        }
    };
    let row = db.orm().query_one_raw(stmt).await.map_err(orm_err)?;
    row.as_ref().map(claimed_from_row).transpose()
}

/// Claim the next due job whose account is not already in-flight.
pub async fn claim_next(
    db: &DbPool,
    now: &str,
    inflight: &InFlight,
) -> Result<Option<ClaimedJob>, sqlx::Error> {
    let mut stmt = Sq::select();
    stmt.columns([jobs::Column::Id, jobs::Column::Payload])
        .from(jobs::Entity)
        .and_where(jobs::Column::Status.eq("pending"))
        .and_where(jobs::Column::RunAt.lte(now.to_owned()))
        .order_by(jobs::Column::RunAt, Order::Asc)
        .order_by(jobs::Column::CreatedAt, Order::Asc);
    let rows = db.orm().query_all(&stmt).await.map_err(orm_err)?;

    for row in &rows {
        let id: String = row.try_get("", "id").map_err(orm_err)?;
        let payload_json: String = row.try_get("", "payload").map_err(orm_err)?;
        let Ok(payload) = serde_json::from_str::<JobPayload>(&payload_json) else {
            continue;
        };
        if let JobPayload::SyncAccount { account_id, .. } = &payload
            && inflight.contains(account_id).await
        {
            continue;
        }
        // CAS claim: exactly one worker flips `pending` → `running`.
        let claimed = jobs::Entity::update_many()
            .col_expr(jobs::Column::Status, Expr::val("running"))
            .col_expr(jobs::Column::UpdatedAt, Expr::val(updated_at_value(db)))
            .filter(
                sea_orm::Condition::all()
                    .add(jobs::Column::Id.eq(id.clone()))
                    .add(jobs::Column::Status.eq("pending")),
            )
            .exec(&db.orm())
            .await
            .map_err(orm_err)?;
        if claimed.rows_affected == 1 {
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
pub async fn reclaim_stale_running(db: &DbPool) -> Result<u64, sqlx::Error> {
    let result = jobs::Entity::update_many()
        .col_expr(jobs::Column::Status, Expr::val("pending"))
        .col_expr(jobs::Column::UpdatedAt, Expr::val(updated_at_value(db)))
        .filter(jobs::Column::Status.eq("running"))
        .exec(&db.orm())
        .await
        .map_err(orm_err)?;
    Ok(result.rows_affected)
}

/// Acquire a worker permit, then claim. The permit is held for the job's
/// lifetime so `running` cannot exceed `SYNC_MAX_CONCURRENT`.
pub async fn try_claim_with_permit(
    db: &DbPool,
    now: &str,
    inflight: &InFlight,
    sem: &Arc<Semaphore>,
) -> Result<Option<(ClaimedJob, OwnedSemaphorePermit)>, sqlx::Error> {
    let Ok(permit) = Arc::clone(sem).try_acquire_owned() else {
        return Ok(None);
    };
    match claim_next(db, now, inflight).await? {
        Some(job) => Ok(Some((job, permit))),
        None => Ok(None),
    }
}

/// Map a sync failure to a fixed, safe category string for `jobs.last_error`.
///
/// Whitelist, not blacklist: raw error text is never persisted, because it can
/// echo credentials, tokens, usernames, or server chatter.
fn sanitize_error(err: &SyncError) -> &'static str {
    match err {
        SyncError::Imap(crate::imap::ImapError::Login(_)) => "auth error",
        SyncError::Imap(crate::imap::ImapError::Timeout) => "IMAP timeout",
        SyncError::Imap(_) => "IMAP error",
        SyncError::Jmap(crate::sync::jmap_client::JmapError::Authentication(_)) => "auth error",
        SyncError::Jmap(_) => "JMAP error",
        SyncError::Smtp(smtp) => smtp.job_category(),
        SyncError::Database(_) => "database error",
        SyncError::Protocol(_) => "protocol error",
        SyncError::Crypto(_) => "credential error",
        _ => "sync failed",
    }
}

async fn mark_completed(db: &DbPool, id: &str) -> Result<(), sqlx::Error> {
    jobs::Entity::update_many()
        .col_expr(jobs::Column::Status, Expr::val("completed"))
        .col_expr(jobs::Column::LastError, Expr::val(Value::String(None)))
        .col_expr(jobs::Column::UpdatedAt, Expr::val(updated_at_value(db)))
        .filter(jobs::Column::Id.eq(id))
        .exec(&db.orm())
        .await
        .map_err(orm_err)?;
    Ok(())
}

async fn mark_failed(db: &DbPool, id: &str, last_error: &str) -> Result<(), sqlx::Error> {
    // Scoped: blanket `ExprTrait` (for `.add`) shadows integer-inherent
    // `.min` / `.max`, so it must not leak into the whole module.
    use sea_orm::sea_query::ExprTrait;
    jobs::Entity::update_many()
        .col_expr(jobs::Column::Status, Expr::val("failed"))
        .col_expr(jobs::Column::LastError, Expr::val(last_error))
        .col_expr(
            jobs::Column::Attempts,
            Expr::col(jobs::Column::Attempts).add(1),
        )
        .col_expr(jobs::Column::UpdatedAt, Expr::val(updated_at_value(db)))
        .filter(jobs::Column::Id.eq(id))
        .exec(&db.orm())
        .await
        .map_err(orm_err)?;
    Ok(())
}

/// Reschedule a transient send failure: bump attempts, set pending + delayed `run_at`.
async fn reschedule_transient(
    db: &DbPool,
    id: &str,
    last_error: &str,
    delay_secs: i64,
) -> Result<(), sqlx::Error> {
    use sea_orm::sea_query::ExprTrait;
    let run_at = (chrono::Utc::now() + chrono::Duration::seconds(delay_secs)).to_rfc3339();
    jobs::Entity::update_many()
        .col_expr(jobs::Column::Status, Expr::val("pending"))
        .col_expr(jobs::Column::LastError, Expr::val(last_error))
        .col_expr(
            jobs::Column::Attempts,
            Expr::col(jobs::Column::Attempts).add(1),
        )
        .col_expr(jobs::Column::RunAt, Expr::val(run_at.as_str()))
        .col_expr(jobs::Column::UpdatedAt, Expr::val(updated_at_value(db)))
        .filter(jobs::Column::Id.eq(id))
        .exec(&db.orm())
        .await
        .map_err(orm_err)?;
    Ok(())
}

const SMTP_TRANSIENT_MAX_ATTEMPTS: i64 = 3;

async fn revert_pending(db: &DbPool, id: &str) -> Result<(), sqlx::Error> {
    jobs::Entity::update_many()
        .col_expr(jobs::Column::Status, Expr::val("pending"))
        .col_expr(jobs::Column::UpdatedAt, Expr::val(updated_at_value(db)))
        .filter(jobs::Column::Id.eq(id))
        .exec(&db.orm())
        .await
        .map_err(orm_err)?;
    Ok(())
}

/// Dispatch a claimed job. Plugin errors mark the job `failed` (no panic).
/// `permit` must already be held; it is released when this future ends.
pub async fn process_job(
    db: &DbPool,
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
                revert_pending(db, &job.id).await?;
                return Ok(());
            }
            app.events.emit(crate::kernel::AppEvent::SyncStarted {
                account_id: account_id.clone(),
            });
            let result = crate::sync::run_account_sync(db, app, &user_id, &account_id).await;
            inflight.finish(&account_id).await;
            match result {
                Ok(_) => {
                    app.events.emit(crate::kernel::AppEvent::SyncComplete {
                        account_id: account_id.clone(),
                    });
                    mark_completed(db, &job.id).await?;
                }
                Err(err) => {
                    let safe = sanitize_error(&err);
                    app.events.emit(crate::kernel::AppEvent::SyncError {
                        account_id: account_id.clone(),
                        error: safe.to_string(),
                    });
                    tracing::warn!(job_id = %job.id, error = %safe, "sync job failed");
                    mark_failed(db, &job.id, safe).await?;
                }
            }
        }
        JobPayload::UnsnoozeMessage { message_id } => {
            // Not part of the audit: absent fields keep stored values, and the
            // original write did not touch `updated_at`.
            message::Entity::update_many()
                .col_expr(
                    message::Column::SnoozedUntil,
                    Expr::val(Value::ChronoDateTimeUtc(None)),
                )
                .filter(message::Column::Id.eq(fk_id_value(db, &message_id)?))
                .exec(&db.orm())
                .await
                .map_err(orm_err)?;
            mark_completed(db, &job.id).await?;
        }
        JobPayload::SendMessage {
            account_id,
            outbound,
        } => {
            handle_send_message(db, app, &job.id, &account_id, &outbound).await?;
        }
    }
    Ok(())
}

/// Run one `SendMessage` payload through the account's configured send plugin.
///
/// Terminal failures mark the job `failed` with a sanitized category;
/// `SMTP transient` / `JMAP transient` categories are rescheduled with
/// capped exponential backoff while attempts remain.
async fn handle_send_message(
    db: &DbPool,
    app: &App,
    job_id: &str,
    account_id: &str,
    outbound: &serde_json::Value,
) -> Result<(), sqlx::Error> {
    let Ok(raw) = serde_json::to_string(outbound) else {
        mark_failed(db, job_id, "invalid outbound payload").await?;
        return Ok(());
    };
    let mut probe = Sq::select();
    probe
        .column(mail_account::Column::SendProtocol)
        .from(mail_account::Entity)
        .and_where(mail_account::Column::Id.eq(fk_id_value(db, account_id)?));
    let protocol: Option<String> = match db.orm().query_one(&probe).await.map_err(orm_err)? {
        Some(row) => row.try_get("", "send_protocol").map_err(orm_err)?,
        None => None,
    };
    let Some(protocol) = protocol.filter(|p| !p.is_empty()) else {
        mark_failed(db, job_id, "send protocol not configured").await?;
        return Ok(());
    };
    let Ok(plugin) = app.send(&protocol) else {
        mark_failed(db, job_id, "unknown send protocol").await?;
        return Ok(());
    };
    match plugin.send(account_id, &raw).await {
        Ok(()) => mark_completed(db, job_id).await?,
        Err(err_cat) => {
            let mut sel = Sq::select();
            sel.column(jobs::Column::Attempts)
                .from(jobs::Entity)
                .and_where(jobs::Column::Id.eq(job_id));
            let attempts: i64 = match db.orm().query_one(&sel).await.map_err(orm_err)? {
                Some(row) => row.try_get("", "attempts").map_err(orm_err)?,
                None => 0,
            };
            let is_transient = err_cat == "SMTP transient" || err_cat == "JMAP transient";
            if is_transient && attempts + 1 < SMTP_TRANSIENT_MAX_ATTEMPTS {
                let delay = 30i64 * (1i64 << attempts.min(4));
                tracing::warn!(
                    job_id = %job_id,
                    attempts,
                    delay_secs = delay,
                    "send job transient failure — reschedule"
                );
                reschedule_transient(db, job_id, &err_cat, delay).await?;
            } else {
                let safe = if err_cat.starts_with("SMTP ") || err_cat.starts_with("JMAP ") {
                    err_cat.as_str()
                } else {
                    "send error"
                };
                tracing::warn!(job_id = %job_id, error = %safe, "send job failed");
                mark_failed(db, job_id, safe).await?;
            }
        }
    }
    Ok(())
}

/// Spawn the job poller after `AuthState` exists. Worker needs the db pool + `Arc<App>`.
pub fn spawn_workers(db: DbPool, app: Arc<App>, max_concurrent: usize) {
    let inflight = Arc::new(InFlight::new());
    let sem = Arc::new(Semaphore::new(max_concurrent.max(1)));
    tokio::spawn(async move {
        if let Err(error) = reclaim_stale_running(&db).await {
            tracing::error!(%error, "failed to reclaim stale running jobs");
        }
        loop {
            let now = chrono::Utc::now().to_rfc3339();
            match try_claim_with_permit(&db, &now, &inflight, &sem).await {
                Ok(Some((job, permit))) => {
                    let db = db.clone();
                    let app = Arc::clone(&app);
                    let inflight = Arc::clone(&inflight);
                    tokio::spawn(async move {
                        if let Err(error) = process_job(&db, &app, &inflight, permit, job).await {
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

    async fn seed_account_with_cursor(db: &DbPool) -> (String, String, String) {
        let pool = sqlite_pool(db);
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
        let db = test_pool().await;
        let now = "2026-08-22T00:00:00+00:00";
        let payload = JobPayload::SyncAccount {
            account_id: "acc-1".into(),
            user_id: "user-1".into(),
        };

        let job_id = enqueue(&db, &payload, now).await.unwrap();
        let claimed = claim_due(&db, now)
            .await
            .unwrap()
            .expect("pending job should be claimed");

        assert_eq!(claimed.id, job_id);
        assert_eq!(claimed.status, "running");

        let status: String = sqlx::query_scalar("SELECT status FROM jobs WHERE id = ?")
            .bind(&job_id)
            .fetch_one(sqlite_pool(&db))
            .await
            .unwrap();
        assert_eq!(status, "running");
    }

    #[tokio::test]
    async fn second_sync_same_account_skipped_while_running() {
        let db = test_pool().await;
        let inflight = InFlight::new();
        let now = "2026-08-22T00:00:00+00:00";
        let payload = JobPayload::SyncAccount {
            account_id: "acc-1".into(),
            user_id: "user-1".into(),
        };

        let first_id = enqueue(&db, &payload, now).await.unwrap();
        let first = claim_due(&db, now).await.unwrap().expect("first job");
        assert_eq!(first.id, first_id);
        assert_eq!(first.status, "running");
        assert!(inflight.try_begin("acc-1").await);

        let second_id = enqueue(&db, &payload, now).await.unwrap();
        let next = claim_next(&db, now, &inflight).await.unwrap();
        assert!(
            next.is_none(),
            "second sync for the same account must be skipped while running"
        );

        let status: String = sqlx::query_scalar("SELECT status FROM jobs WHERE id = ?")
            .bind(&second_id)
            .fetch_one(sqlite_pool(&db))
            .await
            .unwrap();
        assert_eq!(status, "pending");
    }

    #[tokio::test]
    async fn cursor_not_advanced_on_plugin_error() {
        let db = test_pool().await;
        let (user_id, account_id, folder_id) = seed_account_with_cursor(&db).await;
        let now = "2026-08-22T00:00:00+00:00";

        let mut app = App::new();
        app.register_receive(Arc::new(FailingReceive));

        let payload = JobPayload::SyncAccount {
            account_id: account_id.clone(),
            user_id,
        };
        let job_id = enqueue(&db, &payload, now).await.unwrap();
        let claimed = claim_due(&db, now).await.unwrap().expect("due job");

        let mut events = app.events.subscribe();
        let inflight = InFlight::new();
        let sem = Arc::new(Semaphore::new(3));
        let permit = Arc::clone(&sem)
            .try_acquire_owned()
            .expect("test semaphore has a permit");
        process_job(&db, &app, &inflight, permit, claimed)
            .await
            .expect("plugin error must not panic");

        match events.recv().await.expect("started") {
            crate::kernel::AppEvent::SyncStarted { account_id: id } => {
                assert_eq!(id, account_id);
            }
            other => panic!("expected SyncStarted, got {other:?}"),
        }
        match events.recv().await.expect("error") {
            crate::kernel::AppEvent::SyncError {
                account_id: id,
                error,
            } => {
                assert_eq!(id, account_id);
                assert!(!error.to_lowercase().contains("password"));
                assert!(!error.contains("hunter2"));
            }
            other => panic!("expected SyncError, got {other:?}"),
        }

        let row = sqlx::query_as::<_, (String, Option<String>)>(
            "SELECT status, last_error FROM jobs WHERE id = ?",
        )
        .bind(&job_id)
        .fetch_one(sqlite_pool(&db))
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
                .fetch_one(sqlite_pool(&db))
                .await
                .unwrap();
        assert_eq!(cursor, "uid:100");
    }

    #[tokio::test]
    async fn stale_running_job_reclaimed_then_claimable() {
        let db = test_pool().await;
        let now = "2026-08-22T00:00:00+00:00";
        let inflight = InFlight::new();
        let payload = JobPayload::SyncAccount {
            account_id: "acc-stale".into(),
            user_id: "user-1".into(),
        };

        let job_id = enqueue(&db, &payload, now).await.unwrap();
        sqlx::query("UPDATE jobs SET status = 'running' WHERE id = ?")
            .bind(&job_id)
            .execute(sqlite_pool(&db))
            .await
            .unwrap();

        assert!(
            claim_next(&db, now, &inflight).await.unwrap().is_none(),
            "running leftovers must not be claimed before reclaim"
        );

        let reclaimed = reclaim_stale_running(&db).await.unwrap();
        assert_eq!(reclaimed, 1);

        let claimed = claim_next(&db, now, &inflight)
            .await
            .unwrap()
            .expect("reclaimed job should be claimable via claim_next");
        assert_eq!(claimed.id, job_id);
        assert_eq!(claimed.status, "running");
    }

    #[tokio::test]
    async fn claim_does_not_exceed_semaphore_cap() {
        let db = test_pool().await;
        let now = "2026-08-22T00:00:00+00:00";
        let inflight = InFlight::new();
        let sem = Arc::new(Semaphore::new(1));

        for i in 0..3 {
            enqueue(
                &db,
                &JobPayload::SyncAccount {
                    account_id: format!("acc-{i}"),
                    user_id: "user-1".into(),
                },
                now,
            )
            .await
            .unwrap();
        }

        let first = try_claim_with_permit(&db, now, &inflight, &sem)
            .await
            .unwrap()
            .expect("should claim when a permit is available");

        let second = try_claim_with_permit(&db, now, &inflight, &sem)
            .await
            .unwrap();
        assert!(
            second.is_none(),
            "must not mark another job running without a permit"
        );

        let running: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM jobs WHERE status = 'running'")
            .fetch_one(sqlite_pool(&db))
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
            "auth error"
        );
        assert_eq!(
            sanitize_error(&SyncError::Imap(crate::imap::ImapError::Timeout)),
            "IMAP timeout"
        );
        assert_eq!(
            sanitize_error(&SyncError::Jmap(
                crate::sync::jmap_client::JmapError::SessionDiscovery(msg.into())
            )),
            "JMAP error"
        );
        assert_eq!(
            sanitize_error(&SyncError::Crypto(msg.into())),
            "credential error"
        );
        assert_eq!(
            sanitize_error(&SyncError::Smtp(crate::smtp::SmtpError::Credential(
                msg.into()
            ))),
            "SMTP error"
        );
        assert_eq!(
            sanitize_error(&SyncError::Smtp(crate::smtp::SmtpError::Transient(
                msg.into()
            ))),
            "SMTP transient"
        );
        assert_eq!(
            sanitize_error(&SyncError::Smtp(crate::smtp::SmtpError::Permanent(
                msg.into()
            ))),
            "SMTP permanent"
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

    struct OkSend;

    #[async_trait]
    impl crate::protocol::SendPlugin for OkSend {
        fn id(&self) -> &'static str {
            "smtp"
        }

        async fn send(&self, _account_id: &str, _raw: &str) -> Result<(), String> {
            Ok(())
        }
    }

    struct BoomSend;

    #[async_trait]
    impl crate::protocol::SendPlugin for BoomSend {
        fn id(&self) -> &'static str {
            "smtp"
        }

        async fn send(&self, _account_id: &str, _raw: &str) -> Result<(), String> {
            // Simulate sanitized plugin output (real smtp_send maps via job_category).
            Err("SMTP permanent".into())
        }
    }

    async fn process_one(db: &DbPool, app: &App, job: ClaimedJob) {
        let inflight = InFlight::new();
        let sem = Arc::new(Semaphore::new(1));
        let permit = Arc::clone(&sem)
            .try_acquire_owned()
            .expect("test semaphore has a permit");
        process_job(db, app, &inflight, permit, job)
            .await
            .expect("process_job");
    }

    #[tokio::test]
    async fn send_message_job_marks_completed_not_pending() {
        let db = test_pool().await;
        let (_, account_id, _) = seed_account_with_cursor(&db).await;
        let now = "2026-08-22T00:00:00+00:00";

        let mut app = App::new();
        app.register_send(Arc::new(OkSend));

        let payload = JobPayload::SendMessage {
            account_id,
            outbound: serde_json::json!({"to":["a@example.com"],"subject":"hi","body_text":"x"}),
        };
        let job_id = enqueue(&db, &payload, now).await.unwrap();
        let claimed = claim_due(&db, now).await.unwrap().expect("due job");
        process_one(&db, &app, claimed).await;

        let status: String = sqlx::query_scalar("SELECT status FROM jobs WHERE id = ?")
            .bind(&job_id)
            .fetch_one(sqlite_pool(&db))
            .await
            .unwrap();
        assert_eq!(status, "completed");
    }

    #[tokio::test]
    async fn send_message_job_failure_is_terminal_and_sanitized() {
        let db = test_pool().await;
        let (_, account_id, _) = seed_account_with_cursor(&db).await;
        let now = "2026-08-22T00:00:00+00:00";

        let mut app = App::new();
        app.register_send(Arc::new(BoomSend));

        let payload = JobPayload::SendMessage {
            account_id,
            outbound: serde_json::json!({"to":["a@example.com"]}),
        };
        let job_id = enqueue(&db, &payload, now).await.unwrap();
        let claimed = claim_due(&db, now).await.unwrap().expect("due job");
        process_one(&db, &app, claimed).await;

        let row = sqlx::query_as::<_, (String, Option<String>)>(
            "SELECT status, last_error FROM jobs WHERE id = ?",
        )
        .bind(&job_id)
        .fetch_one(sqlite_pool(&db))
        .await
        .unwrap();
        assert_eq!(row.0, "failed");
        let err = row.1.expect("last_error");
        assert_eq!(err, "SMTP permanent");
        assert!(!err.contains("hunter2"), "{err}");
        assert!(!err.to_lowercase().contains("password"), "{err}");
    }

    struct TransientSend;

    #[async_trait]
    impl crate::protocol::SendPlugin for TransientSend {
        fn id(&self) -> &'static str {
            "smtp"
        }

        async fn send(&self, _account_id: &str, _raw: &str) -> Result<(), String> {
            Err("SMTP transient".into())
        }
    }

    #[tokio::test]
    async fn send_message_transient_is_rescheduled() {
        let db = test_pool().await;
        let (_, account_id, _) = seed_account_with_cursor(&db).await;
        let now = "2026-08-22T00:00:00+00:00";

        let mut app = App::new();
        app.register_send(Arc::new(TransientSend));

        let payload = JobPayload::SendMessage {
            account_id,
            outbound: serde_json::json!({"to":["a@example.com"]}),
        };
        let job_id = enqueue(&db, &payload, now).await.unwrap();
        let claimed = claim_due(&db, now).await.unwrap().expect("due job");
        process_one(&db, &app, claimed).await;

        let row = sqlx::query_as::<_, (String, Option<String>, i64)>(
            "SELECT status, last_error, attempts FROM jobs WHERE id = ?",
        )
        .bind(&job_id)
        .fetch_one(sqlite_pool(&db))
        .await
        .unwrap();
        assert_eq!(row.0, "pending");
        assert_eq!(row.1.as_deref(), Some("SMTP transient"));
        assert_eq!(row.2, 1);
    }
}
