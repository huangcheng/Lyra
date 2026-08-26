//! JMAP mailbox fetch loop (IMAP fallback on failure).

use super::imap_loop::run_imap_sync;
use super::store::{
    clear_jmap_cursor, get_folder_id, link_jmap_folder_parent, load_account_sync_row,
    load_jmap_cursor, outcome_from_response, persist_jmap_folder_batch, upsert_jmap_folder,
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
    let Ok(dek) = crate::auth::AuthState::get_user_dek(db, user_id).await else {
        return Err(super::recovery::fail_credential_decrypt(db, account_id).await);
    };
    let row = load_account_sync_row(db, user_id, account_id).await?;
    let credential_json = row.credential.clone();
    let email_address = row.email_address.clone();
    let jmap_base_url = row.jmap_base_url.clone();

    let result = if let Some(ref base_url) = jmap_base_url {
        let Ok(password) = crate::jmap::decrypt_account_password(&credential_json, &dek) else {
            return Err(super::recovery::fail_credential_decrypt(db, account_id).await);
        };
        match run_jmap_sync(db, account_id, base_url, &email_address, &password).await {
            Ok(result) => result,
            Err(e) => {
                tracing::warn!("JMAP sync failed ({e}), falling back to IMAP");
                let Ok(password) = crate::imap::decrypt_account_password(&credential_json, &dek)
                else {
                    return Err(super::recovery::fail_credential_decrypt(db, account_id).await);
                };
                run_imap_sync(db, account_id, &row, &password, false).await?
            }
        }
    } else {
        let Ok(password) = crate::imap::decrypt_account_password(&credential_json, &dek) else {
            return Err(super::recovery::fail_credential_decrypt(db, account_id).await);
        };
        run_imap_sync(db, account_id, &row, &password, false).await?
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
    /// Email/get page size (kept small so one response stays bounded).
    const EMAIL_GET_PAGE: usize = 100;

    // 1. Discover JMAP session
    let client = JmapClient::discover(jmap_base_url, email, password).await?;

    // 2. List mailboxes
    let mailboxes = client.list_mailboxes().await?;
    let mut folders_synced = 0;

    for mb in &mailboxes {
        upsert_jmap_folder(db, account_id, mb).await?;
        folders_synced += 1;
    }
    for mb in &mailboxes {
        if let Some(ref parent_id) = mb.parent_id {
            link_jmap_folder_parent(db, account_id, &mb.id, parent_id).await?;
        }
    }

    // 3. Sync emails per mailbox
    let mut total_new = 0;
    let mut total_updated = 0;

    for mb in &mailboxes {
        let folder_id = get_folder_id(db, account_id, &mb.id).await?;

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
                    query_all_email_ids(&client, &mb.id).await?
                }
                Err(e) => return Err(e.into()),
            }
        } else {
            query_all_email_ids(&client, &mb.id).await?
        };

        if ids.is_empty() {
            persist_jmap_folder_batch(db, account_id, &folder_id, &[], query_state.as_deref())
                .await?;
            continue;
        }

        // Fetch + persist one page at a time: Email/get responses for a huge
        // mailbox in one call would balloon memory, and a crash mid-way just
        // redoes the (idempotent) upserts on the next run.
        for chunk in ids.chunks(EMAIL_GET_PAGE) {
            let emails = client.get_emails(chunk).await?;
            let (n, u) = persist_jmap_folder_batch(
                db,
                account_id,
                &folder_id,
                &emails,
                query_state.as_deref(),
            )
            .await?;
            total_new += n;
            total_updated += u;
        }
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

/// Page through `Email/query` until the mailbox is exhausted.
///
/// The first page's `queryState` must not be committed before every page is
/// read: once it lands, `Email/queryChanges` only ever returns deltas and
/// anything past page one would be unreachable forever (sync spec §4.1).
async fn query_all_email_ids(
    client: &JmapClient,
    mailbox_id: &str,
) -> Result<(Vec<String>, Option<String>), SyncError> {
    const QUERY_PAGE: u32 = 100;
    let mut ids: Vec<String> = Vec::new();
    let mut query_state: Option<String> = None;
    let mut position: u32 = 0;
    loop {
        let page = client
            .query_emails(mailbox_id, Some(position), Some(QUERY_PAGE))
            .await?;
        query_state = page.query_state.or(query_state);
        let fetched = u32::try_from(page.ids.len()).unwrap_or(QUERY_PAGE);
        ids.extend(page.ids);
        if fetched < QUERY_PAGE {
            break;
        }
        position += QUERY_PAGE;
    }
    Ok((ids, query_state))
}
