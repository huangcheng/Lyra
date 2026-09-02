//! IMAP folder fetch loop.

use std::collections::HashSet;

use super::store::{
    AccountSyncRow, get_folder_id, imap_folder_depth, load_account_sync_row, load_cursor,
    mojibake_message_uids, outcome_from_response, persist_imap_folder_batch,
    reconcile_folder_deletions, repair_imap_messages, upsert_folder,
};
use super::types::{SyncError, SyncResponse, error_chain};
use crate::imap::{ImapClient, ImapConfig, ImapError, ImapSecurity};
use crate::protocol::SyncOutcome;
use crate::storage::DbPool;

/// Errors that leave the IMAP byte stream in an unknown state (timeout
/// cancels a mid-flight response; io/parse errors mean desync). Continuing
/// the pass on such a session misparses every later command — end the pass
/// instead; per-folder cursors keep progress and the next poll reconnects.
fn session_may_be_poisoned(err: &ImapError) -> bool {
    matches!(
        err,
        ImapError::Timeout | ImapError::Imap(_) | ImapError::Connection(_) | ImapError::Tls(_)
    )
}

/// Load an IMAP account and run the existing IMAP fetch loop.
pub(crate) async fn imap_sync_account(
    db: &DbPool,
    user_id: &str,
    account_id: &str,
) -> Result<SyncOutcome, SyncError> {
    let Ok(dek) = crate::auth::AuthState::get_user_dek(db, user_id).await else {
        return Err(super::recovery::fail_credential_decrypt(db, account_id).await);
    };
    let row = load_account_sync_row(db, user_id, account_id).await?;
    let oauth = crate::oauth::OAuthRegistry::refresh_configs();
    let secret = match crate::oauth::resolve_mail_access_secret(
        db,
        account_id,
        &row.auth_type,
        &row.credential,
        &dek,
        row.imap_host.as_deref(),
        &oauth,
    )
    .await
    {
        Ok(secret) => secret,
        // Only an undecryptable stored credential justifies deactivation.
        Err(e) if e.is_credential_decrypt() => {
            return Err(super::recovery::fail_credential_decrypt(db, account_id).await);
        }
        // Token endpoint outages / missing server config are retryable.
        Err(e) => {
            tracing::warn!(account_id, error = %e, "mail access secret resolve failed");
            return Err(SyncError::Protocol(e.to_string()));
        }
    };
    let result = run_imap_sync(db, account_id, &row, secret.as_str(), secret.is_xoauth2()).await?;
    Ok(outcome_from_response(&result))
}

/// Run an IMAP-based sync for an account.
///
/// Connects to the IMAP server, lists folders, and syncs messages
/// using UIDVALIDITY + UID cursors, and RFC 7162 CONDSTORE when advertised.
#[allow(clippy::too_many_lines)]
pub(crate) async fn run_imap_sync(
    db: &DbPool,
    account_id: &str,
    row: &AccountSyncRow,
    password: &str,
    xoauth2: bool,
) -> Result<SyncResponse, SyncError> {
    let imap_host = row.imap_host.clone();
    let imap_port = row.imap_port;
    let imap_security = row.imap_security.clone();
    let email_address = row.email_address.clone();

    let host = imap_host.ok_or_else(|| SyncError::Crypto("IMAP host not configured".into()))?;
    let port = u16::try_from(imap_port.unwrap_or(993)).unwrap_or(993);
    let security = match imap_security.as_deref() {
        Some(s) => {
            match crate::netsec::normalize_security_mode(s).map_err(SyncError::InvalidInput)? {
                "starttls" => ImapSecurity::Starttls,
                _ => ImapSecurity::Tls,
            }
        }
        None => ImapSecurity::Tls,
    };

    let config = ImapConfig {
        host,
        port,
        security,
        username: email_address,
        password: zeroize::Zeroizing::new(password.to_string()),
        xoauth2,
    };

    // Connect to IMAP
    let mut client = ImapClient::connect(&config).await?;

    // Sync folders (shallow mailboxes first so parent_id resolves)
    let mut folders = client.list_folders().await?;
    folders.sort_by_key(|f| imap_folder_depth(&f.name, f.delimiter.as_deref()));
    let mut folders_synced = 0;

    for folder in &folders {
        upsert_folder(
            db,
            account_id,
            &folder.name,
            folder.delimiter.as_deref(),
            &folder.attributes,
        )
        .await?;
        folders_synced += 1;
    }

    // Sync messages in each folder
    let mut total_new = 0;
    let mut total_updated = 0;
    let mut total_deleted = 0;

    'folder: for folder in &folders {
        // Hierarchy placeholders (`Archive/` with \Noselect) cannot be SELECTed;
        // one failure here would abort the whole account's sync.
        if folder.attributes.iter().any(|a| a.contains("Noselect")) {
            tracing::debug!(account_id, folder = %folder.name, "skipping \\Noselect folder");
            continue;
        }

        let folder_id = get_folder_id(db, account_id, &folder.name).await?;

        // Per-folder failures skip that folder for this pass — one flaky
        // mailbox (QQ rate-limits aggressively) must not abort the whole
        // account every cycle: cursors are per-folder, so the next pass
        // retries exactly the folders that failed. Connect/login-level
        // failures above remain fatal for the pass.
        let select = match client.select(&folder.name).await {
            Ok(s) => s,
            Err(err) => {
                tracing::warn!(
                    account_id,
                    folder = %folder.name,
                    error = %error_chain(&err),
                    "folder select failed"
                );
                if session_may_be_poisoned(&err) {
                    tracing::warn!(
                        account_id,
                        "IMAP session state uncertain; ending account pass"
                    );
                    break;
                }
                continue;
            }
        };
        let uid_validity = select.uid_validity;
        let highest_modseq = select.highest_modseq.unwrap_or(0);

        // Self-heal rows whose stored text still carries U+FFFD mojibake from
        // before full legacy-charset decoding: re-fetch their envelopes (the
        // upsert conflict path replaces stale text) and clear mojibake bodies
        // so the next open lazily re-parses them. Never fatal to sync.
        match repair_folder_mojibake(db, account_id, &folder_id, &mut client).await {
            Ok(repaired) if repaired > 0 => {
                tracing::info!(account_id, folder = %folder.name, repaired, "repaired mojibake messages");
            }
            Ok(_) => {}
            Err(err) => {
                tracing::warn!(account_id, folder = %folder.name, error = %err, "mojibake repair failed");
            }
        }

        // Reconcile moves/deletions: soft-delete local rows whose UID vanished
        // server-side. Without this, messages moved between folders while the
        // client was offline linger as ghost rows that show an empty body
        // forever. A failed listing skips the reconcile — never delete on
        // incomplete data.
        match client.search_uids(None).await {
            Ok(server_uids) => {
                let server_uids: HashSet<u32> = server_uids.into_iter().collect();
                match reconcile_folder_deletions(db, account_id, &folder_id, &server_uids).await {
                    Ok(n) if n > 0 => {
                        total_deleted += n;
                        tracing::info!(account_id, folder = %folder.name, deleted = n, "reconciled vanished messages");
                    }
                    Ok(_) => {}
                    Err(err) => {
                        tracing::warn!(account_id, folder = %folder.name, error = %err, "deletion reconcile failed");
                    }
                }
            }
            Err(err) => {
                tracing::warn!(account_id, folder = %folder.name, error = %err, "UID listing failed; skipping deletion reconcile");
            }
        }

        // Load existing cursor
        let cursor = load_cursor(db, account_id, &folder_id).await?;
        let uidvalidity_changed = cursor
            .as_ref()
            .is_some_and(|c| c.uid_validity != uid_validity);
        let after_uid = if uidvalidity_changed {
            None
        } else {
            cursor.as_ref().map(|c| c.last_uid)
        };
        let stored_modseq = if uidvalidity_changed {
            0
        } else {
            cursor.as_ref().map_or(0, |c| c.last_modseq)
        };

        let new_uids = match client.search_uids(after_uid).await {
            Ok(uids) => uids,
            Err(err) => {
                tracing::warn!(
                    account_id,
                    folder = %folder.name,
                    error = %error_chain(&err),
                    "UID search failed"
                );
                if session_may_be_poisoned(&err) {
                    tracing::warn!(
                        account_id,
                        "IMAP session state uncertain; ending account pass"
                    );
                    break;
                }
                continue;
            }
        };
        let messages = if client.supports_condstore() && stored_modseq > 0 {
            match client.fetch_changed_since(stored_modseq).await {
                Ok(msgs) => msgs,
                Err(err) => {
                    tracing::warn!(
                        account_id,
                        folder = %folder.name,
                        error = %error_chain(&err),
                        "CONDSTORE fetch failed"
                    );
                    if session_may_be_poisoned(&err) {
                        tracing::warn!(
                            account_id,
                            "IMAP session state uncertain; ending account pass"
                        );
                        break;
                    }
                    continue;
                }
            }
        } else {
            Vec::new()
        };
        let changed_uids: HashSet<u32> = messages.iter().map(|m| m.uid).collect();
        let fetch_uids: Vec<u32> = new_uids
            .into_iter()
            .filter(|uid| !changed_uids.contains(uid))
            .collect();
        let new_modseq = if client.supports_condstore() && highest_modseq > 0 {
            highest_modseq
        } else {
            0
        };

        // New-UID metadata: fetch in chunks and persist each chunk as it
        // lands. Servers that drop connections mid-response (QQ resets
        // every few dozen envelopes) would otherwise lose the whole
        // folder's progress — the cursor only advanced on full-folder
        // success, so a 500-message folder retried from zero forever.
        let mut chunk_cursor = cursor.as_ref().map_or(0, |c| c.last_uid);
        let mut uidvalidity_cleared = false;
        'chunks: for chunk in fetch_uids.chunks(crate::imap::METADATA_CHUNK) {
            match client.fetch_metadata(chunk).await {
                Ok(msgs) if msgs.is_empty() => {}
                Ok(msgs) => {
                    let chunk_max = msgs.iter().map(|m| m.uid).max().unwrap_or(0);
                    chunk_cursor = std::cmp::max(chunk_cursor, chunk_max);
                    let (n, u) = persist_imap_folder_batch(
                        db,
                        account_id,
                        &folder_id,
                        &msgs,
                        uid_validity,
                        chunk_cursor,
                        new_modseq,
                        uidvalidity_changed && !uidvalidity_cleared,
                    )
                    .await?;
                    uidvalidity_cleared = true;
                    total_new += n;
                    total_updated += u;
                }
                Err(err) => {
                    tracing::warn!(
                        account_id,
                        folder = %folder.name,
                        error = %error_chain(&err),
                        "metadata fetch failed after chunk retries"
                    );
                    if session_may_be_poisoned(&err) {
                        tracing::warn!(
                            account_id,
                            "IMAP session state uncertain; ending account pass"
                        );
                        break 'folder;
                    }
                    break 'chunks;
                }
            }
        }

        if messages.is_empty() {
            if uidvalidity_changed && !uidvalidity_cleared {
                persist_imap_folder_batch(
                    db,
                    account_id,
                    &folder_id,
                    &[],
                    uid_validity,
                    0,
                    new_modseq,
                    true,
                )
                .await?;
            } else if new_modseq > stored_modseq {
                persist_imap_folder_batch(
                    db,
                    account_id,
                    &folder_id,
                    &[],
                    uid_validity,
                    chunk_cursor,
                    new_modseq,
                    false,
                )
                .await?;
            }
            continue;
        }

        let max_uid = messages.iter().map(|m| m.uid).max().unwrap_or(0);
        let new_cursor_uid = std::cmp::max(max_uid, chunk_cursor);
        let (n, u) = persist_imap_folder_batch(
            db,
            account_id,
            &folder_id,
            &messages,
            uid_validity,
            new_cursor_uid,
            new_modseq,
            uidvalidity_changed,
        )
        .await?;
        total_new += n;
        total_updated += u;
    }

    // Clean logout
    let _ = client.logout().await;

    Ok(SyncResponse {
        account_id: account_id.to_string(),
        status: "completed".to_string(),
        folders_synced,
        messages_synced: total_new,
        messages_updated: total_updated,
        messages_deleted: total_deleted,
    })
}

/// Re-fetch envelopes for messages whose stored text still carries U+FFFD
/// mojibake (synced before full legacy-charset decoding) and upsert them;
/// [`repair_imap_messages`] also clears mojibake bodies so the next open
/// lazily re-fetches and re-parses them. Runs against the mailbox the caller
/// already selected.
async fn repair_folder_mojibake(
    db: &DbPool,
    account_id: &str,
    folder_id: &str,
    client: &mut ImapClient,
) -> Result<usize, SyncError> {
    let uids = mojibake_message_uids(db, folder_id, 200).await?;
    if uids.is_empty() {
        return Ok(0);
    }
    let fetched = client.fetch_metadata(&uids).await?;
    if fetched.is_empty() {
        return Ok(0);
    }
    repair_imap_messages(db, account_id, folder_id, &fetched).await
}
