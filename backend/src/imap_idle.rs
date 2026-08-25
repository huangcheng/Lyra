//! IMAP IDLE push supervisor (RFC 2177).
//!
//! For each active IMAP receive account whose server advertises IDLE, keeps a
//! long-lived connection on INBOX. On mailbox notify, enqueues `SyncAccount`
//! (same path as the poll scheduler). Accounts without IDLE stay on poll only.
//!
//! See `docs/specs/2026-08-20-lyra-sync-and-protocols-spec.md` §7.3.

#![allow(clippy::doc_markdown)]
#![allow(clippy::missing_errors_doc)]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use sqlx::Row;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use crate::db_row::id_from_row;
use crate::imap::{ImapClient, ImapConfig, ImapSecurity, IdleWatchOutcome};
use crate::jobs::{JobPayload, enqueue};
use crate::scheduler::has_pending_or_running_sync;
use crate::storage::DbPool;

const SUPERVISOR_TICK: Duration = Duration::from_secs(30);
const RECONNECT_DELAY: Duration = Duration::from_secs(15);

struct IdleAccount {
    id: String,
    user_id: String,
    email_address: String,
    credential: String,
    imap_host: Option<String>,
    imap_port: Option<i32>,
    imap_security: Option<String>,
}

/// Start the IDLE supervisor. Spawns one watcher task per eligible IMAP account.
pub fn start_idle_supervisor(db: DbPool) {
    tokio::spawn(async move {
        let running: Arc<Mutex<HashMap<String, JoinHandle<()>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let mut interval = tokio::time::interval(SUPERVISOR_TICK);
        loop {
            interval.tick().await;
            if let Err(error) = reconcile_watchers(&db, &running).await {
                tracing::error!(%error, "IMAP IDLE supervisor reconcile failed");
            }
        }
    });
}

async fn reconcile_watchers(
    db: &DbPool,
    running: &Arc<Mutex<HashMap<String, JoinHandle<()>>>>,
) -> Result<(), sqlx::Error> {
    let accounts = list_imap_idle_candidates(db).await?;
    let wanted: std::collections::HashSet<String> =
        accounts.iter().map(|a| a.id.clone()).collect();

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
                tracing::debug!(account_id = %id, "stopped IMAP IDLE watcher (account gone)");
            }
        }
        // Drop finished tasks so they can be restarted.
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
            run_account_idle_loop(db, account).await;
        });
        map.insert(account_id, handle);
    }

    Ok(())
}

async fn list_imap_idle_candidates(db: &DbPool) -> Result<Vec<IdleAccount>, sqlx::Error> {
    db_fetch_all!(
        db,
        r"
        SELECT id, user_id, email_address, credential,
               imap_host, imap_port, imap_security
        FROM mail_account
        WHERE is_active = ? AND sync_enabled = ?
          AND receive_protocol = ?
        ",
        |row| IdleAccount {
            id: id_from_row(row, "id"),
            user_id: id_from_row(row, "user_id"),
            email_address: row.get("email_address"),
            credential: row.get("credential"),
            imap_host: row.get("imap_host"),
            imap_port: row.get("imap_port"),
            imap_security: row.get("imap_security"),
        },
        true,
        true,
        "imap"
    )
}

async fn run_account_idle_loop(db: DbPool, account: IdleAccount) {
    tracing::info!(account_id = %account.id, "IMAP IDLE watcher starting");
    loop {
        match watch_once(&db, &account).await {
            Ok(IdleWatchOutcome::Unsupported) => {
                tracing::info!(
                    account_id = %account.id,
                    "IMAP server lacks IDLE; watcher exiting (poll remains)"
                );
                return;
            }
            Ok(IdleWatchOutcome::Notified) => {
                if let Err(error) = enqueue_sync_if_idle(&db, &account).await {
                    tracing::warn!(
                        account_id = %account.id,
                        %error,
                        "failed to enqueue sync after IDLE notify"
                    );
                }
                // Brief pause so sync can claim the mailbox before we reconnect.
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            Ok(IdleWatchOutcome::Interrupted) => {
                tokio::time::sleep(RECONNECT_DELAY).await;
            }
            Err(error) => {
                tracing::warn!(
                    account_id = %account.id,
                    error = %error,
                    "IMAP IDLE watch failed; reconnecting"
                );
                tokio::time::sleep(RECONNECT_DELAY).await;
            }
        }
    }
}

async fn watch_once(db: &DbPool, account: &IdleAccount) -> Result<IdleWatchOutcome, crate::imap::ImapError> {
    // Avoid racing a full sync on the same account (both open IMAP sessions).
    match has_pending_or_running_sync(db, &account.id).await {
        Ok(true) => {
            tokio::time::sleep(Duration::from_secs(5)).await;
            return Ok(IdleWatchOutcome::Interrupted);
        }
        Ok(false) => {}
        Err(e) => {
            tracing::warn!(account_id = %account.id, error = %e, "IDLE: job status check failed");
            tokio::time::sleep(RECONNECT_DELAY).await;
            return Ok(IdleWatchOutcome::Interrupted);
        }
    }

    let dek = crate::auth::AuthState::get_user_dek(db, &account.user_id).await?;
    let password = crate::imap::decrypt_account_password(&account.credential, &dek)?;

    let host = account
        .imap_host
        .clone()
        .ok_or_else(|| crate::imap::ImapError::Connection("IMAP host not configured".into()))?;
    let port = u16::try_from(account.imap_port.unwrap_or(993)).unwrap_or(993);
    let security = match account.imap_security.as_deref() {
        Some(s) => match crate::netsec::normalize_security_mode(s) {
            Ok("starttls") => ImapSecurity::Starttls,
            Ok(_) => ImapSecurity::Tls,
            Err(e) => return Err(crate::imap::ImapError::Protocol(e)),
        },
        None => ImapSecurity::Tls,
    };

    let config = ImapConfig {
        host,
        port,
        security,
        username: account.email_address.clone(),
        password: zeroize::Zeroizing::new(password),
    };

    let mut client = ImapClient::connect(&config).await?;
    if !client.supports_idle() {
        return Ok(IdleWatchOutcome::Unsupported);
    }

    client.select("INBOX").await?;
    client.into_idle_watch().await
}

async fn enqueue_sync_if_idle(db: &DbPool, account: &IdleAccount) -> Result<(), sqlx::Error> {
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
    tracing::info!(account_id = %account.id, "enqueued sync from IMAP IDLE");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_watch_outcome_discriminants() {
        assert_ne!(IdleWatchOutcome::Notified, IdleWatchOutcome::Unsupported);
        assert_ne!(IdleWatchOutcome::Interrupted, IdleWatchOutcome::Notified);
    }
}
