//! EventBus-driven fan-out: on `SyncComplete`, diff the account's newest
//! mail and push to every registered subscription of the account's owner.
//!
//! Log hygiene: a push endpoint is a bearer capability, so endpoints (and
//! error strings that may embed them) never appear above debug level here.

use std::sync::Arc;

use sea_orm::sea_query::Query as Sq;
use sea_orm::{ColumnTrait, ConnectionTrait};

use crate::kernel::{App, AppEvent};
use crate::kv::KvStore;
use crate::storage::DbPool;

use super::diff::diff_new_messages;
use super::send::{SendOutcome, send_push};
use super::store::{
    load_baseline, load_or_generate_vapid, load_prefs, load_subscriptions, remove_subscription,
    save_baseline,
};

/// Never fire more than this many individual pushes per sync; the rest fold
/// into one summary push (mirrors the frontend notifier).
const MAX_PER_SYNC: usize = 3;

#[derive(Debug, serde::Serialize)]
struct PushPayload {
    title: String,
    body: String,
    tag: String,
    data: PushData,
}

#[derive(Debug, serde::Serialize)]
struct PushData {
    #[serde(rename = "messageId")]
    message_id: String,
}

/// Dialect-safe id read: TEXT on SQLite/MySQL, native UUID on Postgres.
fn row_id(row: &sea_orm::QueryResult, col: &str) -> Option<String> {
    if let Some(s) = row.try_get::<Option<String>>("", col).ok().flatten() {
        return Some(s);
    }
    row.try_get::<Option<uuid::Uuid>>("", col)
        .ok()
        .flatten()
        .map(|u| u.to_string())
}

/// Account → owner lookup. Returns None for unknown accounts.
async fn account_user_id(db: &DbPool, account_id: &str) -> Option<String> {
    let value = crate::sync::queries::id_value_pub(db, account_id).ok()?;
    let mut stmt = Sq::select();
    stmt.column(crate::entities::mail_account::Column::UserId)
        .from(crate::entities::mail_account::Entity)
        .and_where(crate::entities::mail_account::Column::Id.eq(value));
    let row = db.orm().query_one(&stmt).await.ok()??;
    row_id(&row, "user_id")
}

/// One fan-out pass for a freshly synced account. Public for tests and for
/// the event loop below.
pub(crate) async fn fan_out_account(
    db: &DbPool,
    kv: &Arc<dyn KvStore>,
    client: &reqwest::Client,
    account_id: &str,
    vapid_subject: &str,
) -> Result<(), crate::kv::KvError> {
    let Some(user_id) = account_user_id(db, account_id).await else {
        return Ok(());
    };
    let subs = load_subscriptions(kv, &user_id).await?;
    if subs.is_empty() {
        return Ok(());
    }

    let messages =
        crate::sync::queries::query_user_messages(db, &user_id, None, Some(account_id), None)
            .await
            .map_err(|e| crate::kv::KvError::Internal(e.to_string()))?;
    let baseline = load_baseline(kv, account_id).await?;
    let prefs = load_prefs(kv, &user_id).await?;
    let outcome = diff_new_messages(
        &messages,
        &baseline,
        &prefs.muted_folder_ids,
        &prefs.muted_thread_ids,
    );
    // The baseline advances even when every send below fails, so a transient
    // outage never re-notifies old mail on the next sync.
    save_baseline(kv, account_id, &outcome.new_baseline).await?;
    if outcome.seeded || outcome.fresh.is_empty() {
        return Ok(());
    }

    let vapid = load_or_generate_vapid(kv).await?;

    // Push services cap payloads (~4KB); subjects/senders are unbounded TEXT.
    // Truncate well under the cap so a pathological mail can't 413 the send.
    let clip = |s: &str| -> String { s.chars().take(200).collect() };

    let mut payloads: Vec<PushPayload> = outcome
        .fresh
        .iter()
        .take(MAX_PER_SYNC)
        .map(|c| PushPayload {
            title: if c.title.is_empty() {
                "New message".to_string()
            } else {
                clip(&c.title)
            },
            body: clip(&c.body),
            tag: format!("lyra-{}", c.id),
            data: PushData {
                message_id: c.id.clone(),
            },
        })
        .collect();
    let more = outcome.fresh.len() - payloads.len();
    if more > 0 {
        let title = if prefs.locale == "zh" {
            format!("还有 {more} 封新邮件")
        } else {
            format!("{more} more new messages")
        };
        payloads.push(PushPayload {
            title,
            body: String::new(),
            tag: "lyra-summary".to_string(),
            data: PushData {
                message_id: outcome.fresh[MAX_PER_SYNC].id.clone(),
            },
        });
    }

    for payload in &payloads {
        let json = serde_json::to_string(payload)
            .map_err(|e| crate::kv::KvError::Internal(e.to_string()))?;
        for sub in &subs {
            match send_push(client, &vapid.private_pem, vapid_subject, sub, &json).await {
                Ok(SendOutcome::Delivered) => {}
                Ok(SendOutcome::Gone) => {
                    tracing::info!("push subscription gone; removing");
                    let _ = remove_subscription(kv, &user_id, &sub.endpoint).await;
                }
                Ok(SendOutcome::Unauthorized) => {
                    tracing::error!(
                        "push service rejected VAPID identity; check LYRA_VAPID_SUBJECT"
                    );
                }
                Ok(SendOutcome::Failed(_)) => {
                    // The failure string may embed the endpoint URL (a bearer
                    // capability) — static message only.
                    tracing::debug!("push send failed (best-effort)");
                }
                Err(_) => {
                    tracing::warn!("push message build failed");
                }
            }
        }
    }
    Ok(())
}

/// Subscribe to sync events and fan out forever. Spawned from `main`.
pub(crate) fn spawn_fanout(
    db: DbPool,
    kv: Arc<dyn KvStore>,
    app: &Arc<App>,
    vapid_subject: String,
) {
    let mut rx = app.events.subscribe();
    tokio::spawn(async move {
        let client = reqwest::Client::new();
        loop {
            match rx.recv().await {
                Ok(AppEvent::SyncComplete { account_id }) => {
                    if let Err(e) =
                        fan_out_account(&db, &kv, &client, &account_id, &vapid_subject).await
                    {
                        tracing::warn!(error = %e, account = %account_id, "push fan-out failed");
                    }
                }
                Ok(_) => {}
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}
