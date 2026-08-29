//! JMAP mailbox fetch loop (IMAP fallback on failure).

use std::collections::{HashMap, HashSet};

use super::imap_loop::run_imap_sync;
use super::jmap_client::{JmapEmail, JmapSeam};
use super::store::{
    clear_jmap_cursor, clear_jmap_email_state, delete_jmap_messages_by_external_ids,
    find_folder_id, folder_id_for_role, get_folder_id, link_jmap_folder_parent,
    load_account_sync_row, load_jmap_cursor, load_jmap_email_state, outcome_from_response,
    persist_jmap_folder_batch, save_jmap_email_state, upsert_jmap_folder,
};
use super::types::{SyncError, SyncResponse};
use crate::protocol::SyncOutcome;
use crate::storage::DbPool;

/// `Email/query`/`Email/get` page size (kept small so one response stays bounded).
const QUERY_PAGE: usize = 100;

/// Load a JMAP account and run the JMAP fetch loop.
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
        let Ok(secret) = super::jmap_client::decrypt_account_password(&credential_json, &dek)
        else {
            return Err(super::recovery::fail_credential_decrypt(db, account_id).await);
        };
        match run_jmap_sync(
            db,
            account_id,
            base_url,
            &email_address,
            &secret,
            &row.auth_type,
        )
        .await
        {
            Ok(result) => result,
            Err(e) => {
                // Auth failures poison the cached session; drop it so the next
                // sync reconnects with fresh credentials.
                if let SyncError::Jmap(ref jmap_err) = e
                    && jmap_err.is_auth()
                {
                    JmapSeam::evict(account_id);
                }
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
/// Cached session → `Mailbox/get` → account-level `Email/changes` (keyword /
/// mailbox updates, destroys) → per mailbox `Email/queryChanges` (added →
/// fetch; removed → delete) or paged `Email/query` + `Email/get` batched per
/// page → persist (additive: `thread_id`, `folder_id` moves, removed-ids
/// deletes).
#[allow(clippy::too_many_lines)]
pub(crate) async fn run_jmap_sync(
    db: &DbPool,
    account_id: &str,
    jmap_base_url: &str,
    email: &str,
    secret: &str,
    auth_type: &str,
) -> Result<SyncResponse, SyncError> {
    let seam =
        JmapSeam::connect_for_account(account_id, jmap_base_url, email, secret, auth_type).await?;
    seam.refresh_if_stale().await?;

    // 1. Mailboxes.
    let mailboxes = seam.list_mailboxes().await?;
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

    let mut total_new = 0usize;
    let mut total_updated = 0usize;
    let mut refetched: HashSet<String> = HashSet::new();
    let mut removed: Vec<String> = Vec::new();

    // 2. Account-level Email/changes: keyword/mailbox updates + destroys that
    // per-folder queryChanges cannot see (no membership change).
    let email_state_anchor = folder_id_for_role(db, account_id, "inbox").await?;
    let mut new_email_state: Option<String> = None;
    if let Some(ref anchor) = email_state_anchor
        && let Some(since) = load_jmap_email_state(db, account_id, anchor).await?
    {
        match seam.email_changes(&since).await {
            Ok(changes) => {
                removed.extend(changes.destroyed_ids);
                new_email_state = changes.new_state;
                let (n, u) = refetch_updated_emails(
                    db,
                    account_id,
                    &seam,
                    &changes.updated_ids,
                    &mut refetched,
                )
                .await?;
                total_new += n;
                total_updated += u;
            }
            Err(e) if e.is_stale_query_state() => {
                tracing::info!(
                    account_id,
                    "JMAP Email state expired; clearing email_state cursor"
                );
                clear_jmap_email_state(db, account_id, anchor).await?;
            }
            Err(e) => return Err(e.into()),
        }
    }

    // 3. Per-folder queryChanges (or full query when no/expired cursor).
    for mb in &mailboxes {
        let folder_id = get_folder_id(db, account_id, &mb.id).await?;
        let since_state = load_jmap_cursor(db, account_id, &folder_id).await?;

        let Some(state) = since_state else {
            let (n, u) = full_folder_query(
                db,
                account_id,
                &folder_id,
                &mb.id,
                &seam,
                &mut refetched,
                &mut new_email_state,
            )
            .await?;
            total_new += n;
            total_updated += u;
            continue;
        };
        match seam.query_email_changes(&mb.id, &state).await {
            Ok(changes) => {
                removed.extend(changes.removed_ids.iter().cloned());
                refetched.extend(changes.added_ids.iter().cloned());
                if changes.added_ids.is_empty() {
                    // Advance the cursor even with no additions.
                    persist_jmap_folder_batch(
                        db,
                        account_id,
                        &folder_id,
                        &[],
                        changes.new_query_state.as_deref(),
                    )
                    .await?;
                    continue;
                }
                let mut chunks = changes.added_ids.chunks(QUERY_PAGE).peekable();
                while let Some(chunk) = chunks.next() {
                    let last = chunks.peek().is_none();
                    let (emails, email_state) = seam.get_emails(chunk).await?;
                    if new_email_state.is_none() {
                        new_email_state = email_state;
                    }
                    // The queryState cursor commits only with the LAST chunk.
                    let (n, u) = persist_jmap_folder_batch(
                        db,
                        account_id,
                        &folder_id,
                        &emails,
                        if last {
                            changes.new_query_state.as_deref()
                        } else {
                            None
                        },
                    )
                    .await?;
                    total_new += n;
                    total_updated += u;
                }
            }
            Err(e) if e.is_stale_query_state() => {
                tracing::info!(
                    account_id,
                    folder = %mb.name,
                    "JMAP queryState expired; clearing cursor and running a full query"
                );
                clear_jmap_cursor(db, account_id, &folder_id).await?;
                let (n, u) = full_folder_query(
                    db,
                    account_id,
                    &folder_id,
                    &mb.id,
                    &seam,
                    &mut refetched,
                    &mut new_email_state,
                )
                .await?;
                total_new += n;
                total_updated += u;
            }
            Err(e) => return Err(e.into()),
        }
    }

    // 4. Apply removals/destroys not re-fetched during this run (a message
    // moved between folders re-enters via the other folder's `added`, or via
    // Email/changes `updated`, and must not be deleted).
    let deletions = plan_deletions(removed, &refetched);
    let total_deleted = delete_jmap_messages_by_external_ids(db, account_id, &deletions).await?;

    // 5. Commit the account-level Email state last.
    if let (Some(ref anchor), Some(ref state)) = (email_state_anchor, new_email_state) {
        save_jmap_email_state(db, account_id, anchor, state).await?;
    }

    Ok(SyncResponse {
        account_id: account_id.to_string(),
        status: "completed".into(),
        folders_synced,
        messages_synced: total_new,
        messages_updated: total_updated,
        messages_deleted: total_deleted,
    })
}

/// Page through `Email/query` until the mailbox is exhausted, persisting each
/// page. The `queryState` cursor commits only with the LAST page: once it
/// lands, `Email/queryChanges` returns only deltas, so an early commit would
/// strand every message past the committed page on a crash (sync spec §4.1).
async fn full_folder_query(
    db: &DbPool,
    account_id: &str,
    folder_id: &str,
    mailbox_id: &str,
    seam: &JmapSeam,
    refetched: &mut HashSet<String>,
    new_email_state: &mut Option<String>,
) -> Result<(usize, usize), SyncError> {
    let mut new = 0usize;
    let mut updated = 0usize;
    let mut position = 0usize;
    loop {
        let page = seam
            .query_emails_page(mailbox_id, position, QUERY_PAGE)
            .await?;
        let page_len = page.ids.len();
        refetched.extend(page.ids.iter().cloned());
        if new_email_state.is_none() {
            new_email_state.clone_from(&page.email_state);
        }
        let commit_state = if page_len < QUERY_PAGE {
            page.query_state.as_deref()
        } else {
            None
        };
        let (n, u) =
            persist_jmap_folder_batch(db, account_id, folder_id, &page.emails, commit_state)
                .await?;
        new += n;
        updated += u;
        if page_len < QUERY_PAGE {
            break;
        }
        position += QUERY_PAGE;
    }
    Ok((new, updated))
}

/// Re-fetch `Email/changes`-updated messages and upsert them under the folder
/// of their (first locally-known) mailbox — this is how server-side flag and
/// mailbox changes reach local rows between full queries.
async fn refetch_updated_emails(
    db: &DbPool,
    account_id: &str,
    seam: &JmapSeam,
    updated_ids: &[String],
    refetched: &mut HashSet<String>,
) -> Result<(usize, usize), SyncError> {
    let mut new = 0usize;
    let mut updated = 0usize;
    for chunk in updated_ids.chunks(QUERY_PAGE) {
        let (emails, _email_state) = seam.get_emails(chunk).await?;
        let mut by_folder: HashMap<String, Vec<JmapEmail>> = HashMap::new();
        for email in emails {
            refetched.insert(email.id.clone());
            if let Some(folder_id) = resolve_jmap_email_folder(db, account_id, &email).await? {
                by_folder.entry(folder_id).or_default().push(email);
            }
        }
        for (folder_id, group) in by_folder {
            let (n, u) =
                persist_jmap_folder_batch(db, account_id, &folder_id, &group, None).await?;
            new += n;
            updated += u;
        }
    }
    Ok((new, updated))
}

/// First mailbox of the email that maps to a synced local folder.
async fn resolve_jmap_email_folder(
    db: &DbPool,
    account_id: &str,
    email: &JmapEmail,
) -> Result<Option<String>, SyncError> {
    let Some(serde_json::Value::Object(map)) = &email.mailbox_ids else {
        return Ok(None);
    };
    for mailbox_id in map.keys() {
        if let Some(folder_id) = find_folder_id(db, account_id, mailbox_id).await? {
            return Ok(Some(folder_id));
        }
    }
    Ok(None)
}

/// Removals/destroys minus everything re-fetched this run. Deleting only the
/// difference keeps server-side moves from becoming data loss.
fn plan_deletions(removed: Vec<String>, refetched: &HashSet<String>) -> Vec<String> {
    removed
        .into_iter()
        .filter(|id| !refetched.contains(id))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::plan_deletions;
    use std::collections::HashSet;

    #[test]
    fn plan_deletions_drops_refetched_ids() {
        let refetched: HashSet<String> = ["b".to_owned(), "d".to_owned()].into_iter().collect();
        let removed = vec!["a".to_owned(), "b".to_owned(), "c".to_owned()];
        assert_eq!(
            plan_deletions(removed, &refetched),
            vec!["a".to_owned(), "c".to_owned()]
        );
    }

    #[test]
    fn plan_deletions_empty_when_all_refetched() {
        let refetched: HashSet<String> = ["a".to_owned()].into_iter().collect();
        assert!(plan_deletions(vec!["a".to_owned()], &refetched).is_empty());
    }
}
