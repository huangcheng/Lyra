//! IMAP folder fetch loop.

use super::store::{
    AccountSyncRow, get_folder_id, load_account_sync_row, load_cursor, outcome_from_response,
    persist_imap_folder_batch, upsert_folder,
};
use super::types::{SyncError, SyncResponse};
use crate::imap::{ImapClient, ImapConfig, ImapSecurity};
use crate::protocol::SyncOutcome;
use crate::storage::DbPool;

/// Load an IMAP account and run the existing IMAP fetch loop.
pub(crate) async fn imap_sync_account(
    db: &DbPool,
    user_id: &str,
    account_id: &str,
) -> Result<SyncOutcome, SyncError> {
    let row = load_account_sync_row(db, user_id, account_id).await?;
    let credential_json = row.credential.clone();
    let dek = crate::auth::AuthState::get_user_dek(db, user_id)
        .await
        .map_err(|e| SyncError::Crypto(e.to_string()))?;
    let password = crate::imap::decrypt_account_password(&credential_json, &dek)?;
    let result = run_imap_sync(db, account_id, &row, &password).await?;
    Ok(outcome_from_response(&result))
}

/// Run an IMAP-based sync for an account.
///
/// Connects to the IMAP server, lists folders, and syncs messages
/// using UIDVALIDITY + UID cursors.
#[allow(clippy::too_many_lines)]
pub(crate) async fn run_imap_sync(
    db: &DbPool,
    account_id: &str,
    row: &AccountSyncRow,
    password: &str,
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
    };

    // Connect to IMAP
    let mut client = ImapClient::connect(&config).await?;

    // Sync folders
    let folders = client.list_folders().await?;
    let mut folders_synced = 0;

    for folder in &folders {
        upsert_folder(db, account_id, &folder.name, folder.delimiter.as_deref()).await?;
        folders_synced += 1;
    }

    // Sync messages in each folder
    let mut total_new = 0;
    let mut total_updated = 0;
    let total_deleted = 0;

    for folder in &folders {
        let folder_id = get_folder_id(db, account_id, &folder.name).await?;

        let (uid_validity, _uid_next, _exists) = client.select(&folder.name).await?;

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

        let uids = client.search_uids(after_uid).await?;
        if uids.is_empty() {
            if uidvalidity_changed {
                persist_imap_folder_batch(
                    db,
                    account_id,
                    &folder_id,
                    &[],
                    uid_validity,
                    0,
                    true,
                )
                .await?;
            }
            continue;
        }

        let messages = client.fetch_metadata(&uids).await?;
        let max_uid = messages.iter().map(|m| m.uid).max().unwrap_or(0);
        let new_cursor_uid = if uidvalidity_changed {
            max_uid
        } else {
            std::cmp::max(max_uid, cursor.map_or(0, |c| c.last_uid))
        };
        let (n, u) = persist_imap_folder_batch(
            db,
            account_id,
            &folder_id,
            &messages,
            uid_validity,
            new_cursor_uid,
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
