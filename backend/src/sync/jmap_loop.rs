//! JMAP mailbox fetch loop (IMAP fallback on failure).

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::Duration;

use super::imap_loop::run_imap_sync;
use super::jmap_client::{JmapAttachmentMeta, JmapEmail, JmapMailbox, JmapSeam};
use super::store::{
    clear_jmap_cursor, clear_jmap_email_state, delete_jmap_messages_by_external_ids,
    find_folder_id, first_folder_id, folder_id_for_role, get_folder_id, link_jmap_folder_parent,
    load_account_sync_row, load_jmap_cursor, load_jmap_email_state, outcome_from_response,
    persist_jmap_folder_batch, save_jmap_email_state, upsert_jmap_folder,
};
use super::types::{SyncError, SyncResponse};
use crate::protocol::SyncOutcome;
use crate::storage::DbPool;

/// `Email/query`/`Email/get` page size (kept small so one response stays bounded).
const QUERY_PAGE: usize = 100;

/// In-run retries for transient JMAP failures before the IMAP fallback.
const JMAP_TRANSIENT_ATTEMPTS: u32 = 3;

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
        // Transient failures (transport, 5xx/429, rateLimit/…) retry in-run
        // with a short backoff before we consider the IMAP fallback.
        let jmap_result = {
            let mut attempt = 0u32;
            loop {
                attempt += 1;
                let result = match crate::plugins::data_dir() {
                    Ok(data_dir) => {
                        run_jmap_sync(
                            db,
                            account_id,
                            base_url,
                            &email_address,
                            &secret,
                            &row.auth_type,
                            &data_dir,
                        )
                        .await
                    }
                    Err(e) => Err(SyncError::Internal(e)),
                };
                let retryable = matches!(&result, Err(SyncError::Jmap(j)) if j.is_transient());
                if retryable && attempt < JMAP_TRANSIENT_ATTEMPTS {
                    let backoff = Duration::from_secs(u64::from(attempt) * 2);
                    tracing::warn!(
                        account_id,
                        attempt,
                        "JMAP sync hit a transient error; retrying"
                    );
                    tokio::time::sleep(backoff).await;
                    continue;
                }
                break result;
            }
        };
        match jmap_result {
            Ok(result) => result,
            Err(e) => {
                // Auth failures poison the cached session; drop it so the next
                // sync reconnects with fresh credentials.
                if let SyncError::Jmap(ref jmap_err) = e
                    && jmap_err.is_auth()
                {
                    JmapSeam::evict(account_id);
                }
                if row.auth_type.eq_ignore_ascii_case("bearer") {
                    // A Bearer token (e.g. a Fastmail API token) cannot
                    // authenticate IMAP; falling back would mask the JMAP
                    // error behind a certain IMAP auth failure.
                    return Err(e);
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
    data_dir: &Path,
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
    //
    // The account-level state anchors on the inbox-role folder; without one,
    // fall back to the account's first folder (the `sync_cursor.folder_id` FK
    // just needs *some* folder of the account).
    let mut email_state_anchor = folder_id_for_role(db, account_id, "inbox").await?;
    if email_state_anchor.is_none() {
        email_state_anchor = first_folder_id(db, account_id).await?;
        if email_state_anchor.is_some() {
            tracing::warn!(
                account_id,
                "no inbox-role folder; anchoring JMAP email_state on the first folder"
            );
        } else {
            tracing::warn!(
                account_id,
                "no folders synced; account-level JMAP Email/changes pipeline disabled"
            );
        }
    }
    let mut new_email_state: Option<String> = None;
    if let Some(ref anchor) = email_state_anchor {
        match load_jmap_email_state(db, account_id, anchor).await? {
            Some(since) => match seam.email_changes(&since).await {
                Ok(changes) => {
                    removed.extend(changes.destroyed_ids);
                    new_email_state = changes.new_state;
                    let (n, u) = refetch_updated_emails(
                        db,
                        account_id,
                        &seam,
                        &changes.updated_ids,
                        &mut refetched,
                        data_dir,
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
                    // Re-seed immediately: otherwise a quiet account (no folder
                    // fetches anything this run) would leave the account-level
                    // safety net dark — keyword updates stop flowing and a
                    // message removed from one of several mailboxes could be
                    // hard-deleted locally while still on the server.
                    seed_email_state(&seam, &mailboxes, &mut new_email_state).await?;
                }
                // tooManyChanges currently fails the whole run into the IMAP
                // fallback; it could later be handled as clear-cursor + reseed.
                Err(e) => return Err(e.into()),
            },
            // No cursor ever saved (first sync, or a cleared anchor): seed the
            // state now so Email/changes can resume from it on the next run.
            None => seed_email_state(&seam, &mailboxes, &mut new_email_state).await?,
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
                data_dir,
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
                    persist_and_download(
                        db,
                        account_id,
                        &folder_id,
                        &[],
                        changes.new_query_state.as_deref(),
                        &seam,
                        data_dir,
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
                    let (n, u) = persist_and_download(
                        db,
                        account_id,
                        &folder_id,
                        &emails,
                        if last {
                            changes.new_query_state.as_deref()
                        } else {
                            None
                        },
                        &seam,
                        data_dir,
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
                    data_dir,
                )
                .await?;
                total_new += n;
                total_updated += u;
            }
            // tooManyChanges currently fails the whole run into the IMAP
            // fallback; it could later be handled as clear-cursor + full query.
            Err(e) => return Err(e.into()),
        }
    }

    // 4. Apply removals/destroys not re-fetched during this run (a message
    // moved between folders re-enters via the other folder's `added`, or via
    // Email/changes `updated`, and must not be deleted).
    let deletions = plan_deletions(removed, &refetched);
    let total_deleted = delete_jmap_messages_by_external_ids(db, account_id, &deletions).await?;

    // 5. Commit the account-level Email state last.
    // WARNING: committing it earlier (or per-folder) breaks the deletion
    // safety net — destroys from Email/changes must first be validated
    // against this run's refetches (step 4) before the state advances past
    // them, or a move could be misread as a deletion.
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
#[allow(clippy::too_many_arguments)]
async fn full_folder_query(
    db: &DbPool,
    account_id: &str,
    folder_id: &str,
    mailbox_id: &str,
    seam: &JmapSeam,
    refetched: &mut HashSet<String>,
    new_email_state: &mut Option<String>,
    data_dir: &Path,
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
        let (n, u) = persist_and_download(
            db,
            account_id,
            folder_id,
            &page.emails,
            commit_state,
            seam,
            data_dir,
        )
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

/// Seed the account-level `Email` state from a limit-1 `Email/query` page of
/// the first mailbox. Even an empty mailbox yields the batched `Email/get`
/// response's `state` (a required field per RFC 8621), so the seed works on
/// quiet accounts. On the rare `maxCallsInRequest < 2` split path an empty
/// page carries no state; the cursor then stays unset and the seed retries
/// next run.
async fn seed_email_state(
    seam: &JmapSeam,
    mailboxes: &[JmapMailbox],
    new_email_state: &mut Option<String>,
) -> Result<(), SyncError> {
    let Some(first) = mailboxes.first() else {
        return Ok(());
    };
    let page = seam.query_emails_page(&first.id, 0, 1).await?;
    if new_email_state.is_none() {
        *new_email_state = page.email_state;
    }
    Ok(())
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
    data_dir: &Path,
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
                persist_and_download(db, account_id, &folder_id, &group, None, seam, data_dir)
                    .await?;
            new += n;
            updated += u;
        }
    }
    Ok((new, updated))
}

/// Cap per attachment blob (mirrors the IMAP lazy-fetch body cap).
const MAX_ATTACHMENT_DOWNLOAD_BYTES: u64 = 25 * 1024 * 1024;

/// Persist one page, then download attachments for newly-inserted messages
/// (blob bytes land in the content-addressed store under `data_dir`).
async fn persist_and_download(
    db: &DbPool,
    account_id: &str,
    folder_id: &str,
    emails: &[JmapEmail],
    query_state: Option<&str>,
    seam: &JmapSeam,
    data_dir: &Path,
) -> Result<(usize, usize), SyncError> {
    let (new, updated, persisted) =
        persist_jmap_folder_batch(db, account_id, folder_id, emails, query_state).await?;
    // `persisted` is in the same order as `emails` (zip pairs them exactly).
    for (email, persisted) in emails.iter().zip(persisted.iter()) {
        if !persisted.was_new || email.attachments_meta.is_empty() {
            continue;
        }
        download_attachments(
            db,
            account_id,
            &persisted.local_id,
            &email.attachments_meta,
            seam,
            data_dir,
        )
        .await?;
    }
    Ok((new, updated))
}

/// Download one blob, retrying transient transport failures a few times
/// (Fastmail's CDN host drops connections often enough to matter on a first
/// sync of a large mailbox).
async fn download_blob_with_retry(
    seam: &JmapSeam,
    meta: &JmapAttachmentMeta,
) -> Result<Vec<u8>, super::jmap_client::JmapError> {
    let mut attempt = 0u32;
    loop {
        attempt += 1;
        match seam.download_blob(&meta.blob_id).await {
            Err(e) if e.is_transient() && attempt < JMAP_TRANSIENT_ATTEMPTS => {
                tracing::warn!(blob_id = %meta.blob_id, attempt, "JMAP blob download failed transiently; retrying");
                tokio::time::sleep(Duration::from_secs(u64::from(attempt))).await;
            }
            result => return result,
        }
    }
}

/// Download one message's attachment blobs into the blob store and persist
/// the attachment rows. Per-blob failures mark `flags.fetch_error` and never
/// abort the sync.
async fn download_attachments(
    db: &DbPool,
    account_id: &str,
    message_id: &str,
    attachments: &[JmapAttachmentMeta],
    seam: &JmapSeam,
    data_dir: &Path,
) -> Result<(), SyncError> {
    let mut extracted = Vec::new();
    for meta in attachments {
        if meta.size > MAX_ATTACHMENT_DOWNLOAD_BYTES {
            tracing::warn!(
                message_id,
                blob_id = %meta.blob_id,
                size = meta.size,
                "skipping oversized JMAP attachment"
            );
            super::recovery::mark_message_fetch_error(db, message_id, "attachment too large")
                .await?;
            continue;
        }
        match download_blob_with_retry(seam, meta).await {
            // The crate's `download()` buffers the whole body and trusts the
            // server-declared `size` — re-check the real byte count.
            Ok(bytes) if bytes.len() as u64 > MAX_ATTACHMENT_DOWNLOAD_BYTES => {
                tracing::warn!(
                    message_id,
                    blob_id = %meta.blob_id,
                    size = bytes.len(),
                    "downloaded JMAP attachment exceeds the size cap; discarding"
                );
                super::recovery::mark_message_fetch_error(
                    db,
                    message_id,
                    "attachment exceeds size cap",
                )
                .await?;
            }
            Ok(bytes) => extracted.push(crate::imap::ExtractedAttachment {
                filename: meta.filename.clone(),
                content_type: meta.content_type.clone(),
                data: bytes,
                content_id: meta.content_id.clone(),
                is_inline: meta.is_inline,
            }),
            Err(error) => {
                tracing::warn!(message_id, blob_id = %meta.blob_id, %error, "JMAP attachment download failed");
                super::recovery::mark_message_fetch_error(
                    db,
                    message_id,
                    "attachment download failed",
                )
                .await?;
            }
        }
    }
    if !extracted.is_empty() {
        // The message row and cursor are already committed, and `was_new`
        // gating means this message is never re-downloaded — a persist failure
        // would leave a permanent, invisible attachment gap, so flag the row
        // before propagating the error.
        if let Err(error) =
            super::http::persist_attachments(db, data_dir, account_id, message_id, &extracted).await
        {
            if let Err(mark_error) = super::recovery::mark_message_fetch_error(
                db,
                message_id,
                "attachment persist failed",
            )
            .await
            {
                tracing::warn!(message_id, %mark_error, "failed to flag attachment persist failure");
            }
            return Err(error);
        }
    }
    Ok(())
}

/// First mailbox of the email that maps to a synced local folder.
///
/// A message in several mailboxes lands under the first matching map key
/// (JSON object iteration order is arbitrary). A move-out only updates the
/// row's `folder_id`; the vacated folder's counts self-heal on its next
/// persisted batch.
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
