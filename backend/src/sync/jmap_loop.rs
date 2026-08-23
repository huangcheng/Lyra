//! JMAP mailbox fetch loop (IMAP fallback on failure).

use super::imap_loop::run_imap_sync;
use super::store::{
    get_folder_id, load_account_sync_row, load_jmap_cursor, clear_jmap_cursor,
    outcome_from_response, persist_jmap_folder_batch, upsert_folder,
};
use super::types::{SyncError, SyncResponse};
use crate::jmap::JmapClient;
use crate::protocol::SyncOutcome;
use crate::storage::DbPool;

/// Load a JMAP account and run the existing JMAP fetch loop.
///
/// JMAP-then-IMAP fallback stays inside this plugin path, not core dispatch.
pub(crate) async fn jmap_sync_account(
    db: &DbPool,
    user_id: &str,
    account_id: &str,
) -> Result<SyncOutcome, SyncError> {
    let row = load_account_sync_row(db, user_id, account_id).await?;
    let credential_json = row.credential.clone();
    let email_address = row.email_address.clone();
    let jmap_base_url = row.jmap_base_url.clone();
    let dek = crate::auth::AuthState::get_user_dek(db, user_id)
        .await
        .map_err(|e| SyncError::Crypto(e.to_string()))?;

    let result = if let Some(ref base_url) = jmap_base_url {
        let password = crate::jmap::decrypt_account_password(&credential_json, &dek)?;
        match run_jmap_sync(db, account_id, base_url, &email_address, &password).await {
            Ok(result) => result,
            Err(e) => {
                tracing::warn!("JMAP sync failed ({e}), falling back to IMAP");
                let password = crate::imap::decrypt_account_password(&credential_json, &dek)?;
                run_imap_sync(db, account_id, &row, &password).await?
            }
        }
    } else {
        let password = crate::imap::decrypt_account_password(&credential_json, &dek)?;
        run_imap_sync(db, account_id, &row, &password).await?
    };
    Ok(outcome_from_response(&result))
}

/// Run a JMAP sync for an account.
///
/// Discovers the JMAP session, lists mailboxes, queries emails,
/// and upserts them into the database.
pub(crate) async fn run_jmap_sync(
    db: &DbPool,
    account_id: &str,
    jmap_base_url: &str,
    email: &str,
    password: &str,
) -> Result<SyncResponse, SyncError> {
    // 1. Discover JMAP session
    let client = JmapClient::discover(jmap_base_url, email, password).await?;

    // 2. List mailboxes
    let mailboxes = client.list_mailboxes().await?;
    let mut folders_synced = 0;

    for mb in &mailboxes {
        upsert_folder(db, account_id, &mb.name, mb.role.as_deref()).await?;
        folders_synced += 1;
    }

    // 3. Sync emails per mailbox
    let mut total_new = 0;
    let mut total_updated = 0;

    for mb in &mailboxes {
        let folder_id = get_folder_id(db, account_id, &mb.name).await?;

        // Load stored JMAP queryState for Email/queryChanges; fall back to a
        // full Email/query when the token is expired or missing.
        let since_state = load_jmap_cursor(db, account_id, &folder_id).await?;

        let (ids, query_state) = if let Some(ref state) = since_state {
            match client.query_email_changes(&mb.id, state).await {
                Ok(changes) => (changes.added_ids, changes.new_query_state),
                Err(e) if e.is_stale_query_state() => {
                    tracing::info!(
                        account_id,
                        folder = %mb.name,
                        "JMAP queryState expired; clearing cursor and running a full query"
                    );
                    clear_jmap_cursor(db, account_id, &folder_id).await?;
                    let full = client.query_emails(&mb.id, Some(100)).await?;
                    (full.ids, full.query_state)
                }
                Err(e) => return Err(e.into()),
            }
        } else {
            let full = client.query_emails(&mb.id, Some(100)).await?;
            (full.ids, full.query_state)
        };

        if ids.is_empty() {
            persist_jmap_folder_batch(
                db,
                account_id,
                &folder_id,
                &[],
                query_state.as_deref(),
            )
            .await?;
            continue;
        }

        let emails = client.get_emails(&ids).await?;
        let (n, u) =
            persist_jmap_folder_batch(db, account_id, &folder_id, &emails, query_state.as_deref())
                .await?;
        total_new += n;
        total_updated += u;
    }

    Ok(SyncResponse {
        account_id: account_id.to_string(),
        status: "completed".into(),
        folders_synced,
        messages_synced: total_new,
        messages_updated: total_updated,
        messages_deleted: 0,
    })
}
