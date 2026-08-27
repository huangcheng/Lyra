//! JMAP EventSource push supervisor (RFC 8620 §7.3).
//!
//! For each active JMAP receive account whose session exposes `eventSourceUrl`,
//! keeps an authenticated SSE connection. On `state` push, enqueues
//! `SyncAccount`. Accounts without EventSource stay on poll only.
//!
//! See `docs/specs/2026-08-20-lyra-sync-and-protocols-spec.md` §6.3.

#![allow(clippy::doc_markdown)]
#![allow(clippy::missing_errors_doc)]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use sea_orm::sea_query::{Query as Sq, SelectStatement};
use sea_orm::{ColumnTrait, ConnectionTrait, QueryResult};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use crate::entities::mail_account;
use crate::jmap::{EventSourceOutcome, JmapClient};
use crate::jobs::{JobPayload, enqueue};
use crate::scheduler::has_pending_or_running_sync;
use crate::storage::DbPool;

const SUPERVISOR_TICK: Duration = Duration::from_secs(30);
const RECONNECT_DELAY: Duration = Duration::from_secs(15);

struct PushAccount {
    id: String,
    user_id: String,
    email_address: String,
    credential: String,
    jmap_base_url: String,
}

/// Start the JMAP EventSource supervisor.
pub fn start_jmap_push_supervisor(db: DbPool) {
    tokio::spawn(async move {
        let running: Arc<Mutex<HashMap<String, JoinHandle<()>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let mut interval = tokio::time::interval(SUPERVISOR_TICK);
        loop {
            interval.tick().await;
            if let Err(error) = reconcile_watchers(&db, &running).await {
                tracing::error!(%error, "JMAP EventSource supervisor reconcile failed");
            }
        }
    });
}

async fn reconcile_watchers(
    db: &DbPool,
    running: &Arc<Mutex<HashMap<String, JoinHandle<()>>>>,
) -> Result<(), sqlx::Error> {
    let accounts = list_jmap_push_candidates(db).await?;
    let wanted: std::collections::HashSet<String> = accounts.iter().map(|a| a.id.clone()).collect();

    {
        let mut map = running.lock().await;
        let stale: Vec<String> = map
            .keys()
            .filter(|id| !wanted.contains(*id))
            .cloned()
            .collect();
        for id in stale {
            if let Some(handle) = map.remove(&id) {
                handle.abort();
                tracing::debug!(account_id = %id, "stopped JMAP EventSource watcher");
            }
        }
        let finished: Vec<String> = map
            .iter()
            .filter(|(_, h)| h.is_finished())
            .map(|(id, _)| id.clone())
            .collect();
        for id in finished {
            map.remove(&id);
        }
    }

    for account in accounts {
        let mut map = running.lock().await;
        if map.contains_key(&account.id) {
            continue;
        }
        let db = db.clone();
        let account_id = account.id.clone();
        let handle = tokio::spawn(async move {
            run_account_push_loop(db, account).await;
        });
        map.insert(account_id, handle);
    }

    Ok(())
}

// ── SeaORM plumbing (entity query on `db.orm()`) ─────────────────────
//
// Ids are TEXT on SQLite and native UUID on Postgres; row helpers decode
// both shapes where the macro layer used `id_from_row`.

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

/// Decode an id column from either engine (TEXT on SQLite, UUID on Postgres).
fn row_id(row: &QueryResult, col: &str) -> Result<String, sqlx::Error> {
    if let Ok(text) = row.try_get::<Option<String>>("", col) {
        return Ok(text.unwrap_or_default());
    }
    row.try_get::<Option<uuid::Uuid>>("", col)
        .map_err(orm_err)
        .map(|opt| opt.map_or_else(String::new, |u| u.to_string()))
}

async fn list_jmap_push_candidates(db: &DbPool) -> Result<Vec<PushAccount>, sqlx::Error> {
    let mut stmt: SelectStatement = Sq::select();
    stmt.columns([
        mail_account::Column::Id,
        mail_account::Column::UserId,
        mail_account::Column::EmailAddress,
        mail_account::Column::Credential,
        mail_account::Column::JmapBaseUrl,
    ])
    .from(mail_account::Entity)
    .and_where(mail_account::Column::IsActive.eq(true))
    .and_where(mail_account::Column::SyncEnabled.eq(true))
    .and_where(mail_account::Column::ReceiveProtocol.eq("jmap"));

    let rows = db.orm().query_all(&stmt).await.map_err(orm_err)?;
    let tuples: Vec<(String, String, String, String, Option<String>)> = rows
        .iter()
        .map(|row| {
            Ok((
                row_id(row, "id")?,
                row_id(row, "user_id")?,
                row.try_get("", "email_address").map_err(orm_err)?,
                row.try_get("", "credential").map_err(orm_err)?,
                row.try_get("", "jmap_base_url").map_err(orm_err)?,
            ))
        })
        .collect::<Result<Vec<_>, sqlx::Error>>()?;

    Ok(tuples
        .into_iter()
        .filter_map(|(id, user_id, email_address, credential, jmap_base_url)| {
            let jmap_base_url = jmap_base_url.filter(|u| !u.is_empty())?;
            Some(PushAccount {
                id,
                user_id,
                email_address,
                credential,
                jmap_base_url,
            })
        })
        .collect())
}

async fn run_account_push_loop(db: DbPool, account: PushAccount) {
    tracing::info!(account_id = %account.id, "JMAP EventSource watcher starting");
    loop {
        match watch_once(&db, &account).await {
            Ok(EventSourceOutcome::Unsupported) => {
                tracing::info!(
                    account_id = %account.id,
                    "JMAP session has no eventSourceUrl; watcher exiting (poll remains)"
                );
                return;
            }
            Ok(EventSourceOutcome::StateChanged) => {
                if let Err(error) = enqueue_sync_if_idle(&db, &account).await {
                    tracing::warn!(
                        account_id = %account.id,
                        %error,
                        "failed to enqueue sync after JMAP push"
                    );
                }
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            Ok(EventSourceOutcome::StreamEnded) => {
                tracing::debug!(account_id = %account.id, "JMAP EventSource ended; reconnecting");
                tokio::time::sleep(RECONNECT_DELAY).await;
            }
            Err(error) => {
                tracing::warn!(
                    account_id = %account.id,
                    error = %error,
                    "JMAP EventSource failed; reconnecting"
                );
                tokio::time::sleep(RECONNECT_DELAY).await;
            }
        }
    }
}

async fn watch_once(
    db: &DbPool,
    account: &PushAccount,
) -> Result<EventSourceOutcome, crate::jmap::JmapError> {
    match has_pending_or_running_sync(db, &account.id).await {
        Ok(true) => {
            tokio::time::sleep(Duration::from_secs(5)).await;
            return Ok(EventSourceOutcome::StreamEnded);
        }
        Ok(false) => {}
        Err(e) => {
            tracing::warn!(account_id = %account.id, error = %e, "JMAP push: job status check failed");
            tokio::time::sleep(RECONNECT_DELAY).await;
            return Ok(EventSourceOutcome::StreamEnded);
        }
    }

    let dek = crate::auth::AuthState::get_user_dek(db, &account.user_id).await?;
    let password = crate::jmap::decrypt_account_password(&account.credential, &dek)?;
    let client =
        JmapClient::discover(&account.jmap_base_url, &account.email_address, &password).await?;
    client.wait_event_source_state().await
}

async fn enqueue_sync_if_idle(db: &DbPool, account: &PushAccount) -> Result<(), sqlx::Error> {
    if has_pending_or_running_sync(db, &account.id).await? {
        return Ok(());
    }
    let run_at = chrono::Utc::now().to_rfc3339();
    enqueue(
        db,
        &JobPayload::SyncAccount {
            account_id: account.id.clone(),
            user_id: account.user_id.clone(),
        },
        &run_at,
    )
    .await?;
    tracing::info!(account_id = %account.id, "enqueued sync from JMAP EventSource");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_source_outcome_discriminants() {
        assert_ne!(
            EventSourceOutcome::StateChanged,
            EventSourceOutcome::Unsupported
        );
    }
}
