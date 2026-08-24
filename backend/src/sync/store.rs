//! Folder/message persistence, cursors, and folder-batch transactions.

use sqlx::Row;
use uuid::Uuid;

use super::types::{SyncError, SyncResponse};
use crate::db_row::{id_param, message_date_param, opt_json_param};
use crate::imap::ImapMessage;
use crate::protocol::SyncOutcome;
use crate::sanitize::persist_body_html;
use crate::storage::{DbPool, DbTxn};

pub(crate) struct AccountSyncRow {
    pub(crate) email_address: String,
    pub(crate) credential: String,
    pub(crate) imap_host: Option<String>,
    pub(crate) imap_port: Option<i32>,
    pub(crate) imap_security: Option<String>,
    pub(crate) jmap_base_url: Option<String>,
}

pub(crate) async fn load_account_sync_row(
    db: &DbPool,
    user_id: &str,
    account_id: &str,
) -> Result<AccountSyncRow, SyncError> {
    db_fetch_optional!(
        db,
        r"
        SELECT id, email_address, protocol, credential,
               imap_host, imap_port, imap_security,
               jmap_base_url,
               is_active, sync_enabled
        FROM mail_account
        WHERE id = ? AND user_id = ?
        ",
        |row| AccountSyncRow {
            email_address: row.get("email_address"),
            credential: row.get("credential"),
            imap_host: row.get("imap_host"),
            imap_port: row.get("imap_port"),
            imap_security: row.get("imap_security"),
            jmap_base_url: row.get("jmap_base_url"),
        },
        &id_param(db, account_id)?,
        &id_param(db, user_id)?
    )?
    .ok_or(SyncError::AccountNotFound)
}

pub(crate) fn outcome_from_response(result: &SyncResponse) -> SyncOutcome {
    SyncOutcome {
        folders_synced: u32::try_from(result.folders_synced).unwrap_or(u32::MAX),
        messages_synced: u32::try_from(result.messages_synced).unwrap_or(u32::MAX),
    }
}

// ── Database helpers ────────────────────────────────────────────────

/// Split an IMAP wire mailbox path into `(parent_wire, leaf_wire)`.
pub(crate) fn split_imap_folder_path<'a>(
    wire_name: &'a str,
    delimiter: Option<&str>,
) -> (Option<&'a str>, &'a str) {
    let Some(delim) = delimiter.filter(|d| !d.is_empty()) else {
        return (None, wire_name);
    };
    wire_name
        .rsplit_once(delim)
        .map_or((None, wire_name), |(parent, leaf)| {
            if parent.is_empty() {
                (None, leaf)
            } else {
                (Some(parent), leaf)
            }
        })
}

/// Leaf display name for an IMAP mailbox (Modified UTF-7 decoded).
pub(crate) fn imap_folder_display_name(wire_name: &str, delimiter: Option<&str>) -> String {
    let (_, leaf) = split_imap_folder_path(wire_name, delimiter);
    crate::imap::decode_imap_mailbox_name(leaf)
}

/// Depth of an IMAP mailbox path (0 = top-level). Used to upsert parents first.
pub(crate) fn imap_folder_depth(wire_name: &str, delimiter: Option<&str>) -> usize {
    delimiter
        .filter(|d| !d.is_empty())
        .map_or(0, |d| wire_name.matches(d).count())
}

async fn resolve_folder_id_by_external_id(
    db: &DbPool,
    account_id: &str,
    external_id: &str,
) -> Result<Option<String>, SyncError> {
    db_id_optional!(
        db,
        "SELECT id FROM folder WHERE account_id = ? AND external_id = ?",
        &id_param(db, account_id)?,
        external_id
    )
    .map_err(SyncError::from)
}

/// Upsert an IMAP folder by `(account_id, wire_name)`.
///
/// `wire_name` is the encoded IMAP mailbox name (Modified UTF-7 on the wire).
/// It is stored in `external_id` for SELECT/LIST; `name` is the decoded leaf
/// segment; `parent_id` links to the parent mailbox when a delimiter is present.
pub(crate) async fn upsert_folder(
    db: &DbPool,
    account_id: &str,
    wire_name: &str,
    delimiter: Option<&str>,
) -> Result<(), SyncError> {
    let display_name = imap_folder_display_name(wire_name, delimiter);
    let role = infer_folder_role(&display_name);
    let external_id = wire_name;
    let account_bind = id_param(db, account_id)?;
    let (parent_wire, _) = split_imap_folder_path(wire_name, delimiter);
    let parent_id = if let Some(parent_wire) = parent_wire {
        resolve_folder_id_by_external_id(db, account_id, parent_wire).await?
    } else {
        None
    };
    let parent_bind = crate::db_row::opt_id_param(db, parent_id.as_deref())?;

    // Try to find existing folder
    let existing: Option<String> = db_id_optional!(
        db,
        "SELECT id FROM folder WHERE account_id = ? AND external_id = ?",
        &account_bind,
        external_id
    )?;

    if let Some(id) = existing {
        db_execute!(
            db,
            "UPDATE folder SET name = ?, parent_id = ?, updated_at = datetime('now') WHERE id = ?",
            &display_name,
            &parent_bind,
            &id_param(db, &id)?
        )?;
    } else {
        let id = Uuid::new_v7(uuid::Timestamp::now(uuid::NoContext)).to_string();
        db_execute!(
            db,
            r"
            INSERT INTO folder (id, account_id, external_id, name, role, parent_id, sort_order)
            VALUES (?, ?, ?, ?, ?, ?, 0)
            ",
            &id_param(db, &id)?,
            &account_bind,
            external_id,
            &display_name,
            role,
            &parent_bind
        )?;
    }

    Ok(())
}

/// Upsert a JMAP mailbox. Uses mailbox `id` as `external_id`.
pub(crate) async fn upsert_jmap_folder(
    db: &DbPool,
    account_id: &str,
    mailbox: &crate::jmap::JmapMailbox,
) -> Result<(), SyncError> {
    let role = mailbox
        .role
        .as_deref()
        .or_else(|| infer_folder_role(&mailbox.name));
    let account_bind = id_param(db, account_id)?;
    let external_id = mailbox.id.as_str();

    let existing: Option<String> = db_id_optional!(
        db,
        "SELECT id FROM folder WHERE account_id = ? AND external_id = ?",
        &account_bind,
        external_id
    )?;

    if let Some(id) = existing {
        db_execute!(
            db,
            "UPDATE folder SET name = ?, role = ?, updated_at = datetime('now') WHERE id = ?",
            &mailbox.name,
            role,
            &id_param(db, &id)?
        )?;
    } else {
        let id = Uuid::new_v7(uuid::Timestamp::now(uuid::NoContext)).to_string();
        db_execute!(
            db,
            r"
            INSERT INTO folder (id, account_id, external_id, name, role, sort_order)
            VALUES (?, ?, ?, ?, ?, 0)
            ",
            &id_param(db, &id)?,
            &account_bind,
            external_id,
            &mailbox.name,
            role
        )?;
    }

    Ok(())
}

/// Wire JMAP `parentId` after all mailboxes exist locally.
pub(crate) async fn link_jmap_folder_parent(
    db: &DbPool,
    account_id: &str,
    child_external_id: &str,
    parent_external_id: &str,
) -> Result<(), SyncError> {
    let child_id = get_folder_id(db, account_id, child_external_id).await?;
    let parent_id = get_folder_id(db, account_id, parent_external_id).await?;
    db_execute!(
        db,
        "UPDATE folder SET parent_id = ?, updated_at = datetime('now') WHERE id = ?",
        &id_param(db, &parent_id)?,
        &id_param(db, &child_id)?
    )?;
    Ok(())
}

/// Get the local folder ID by `external_id`.
pub(crate) async fn get_folder_id(db: &DbPool, account_id: &str, name: &str) -> Result<String, SyncError> {
    db_id_optional!(
        db,
        "SELECT id FROM folder WHERE account_id = ? AND external_id = ?",
        &id_param(db, account_id)?,
        name
    )?
    .ok_or_else(|| SyncError::Database(sqlx::Error::RowNotFound))
}

/// Infer folder role from name.
pub(crate) fn infer_folder_role(name: &str) -> Option<&'static str> {
    let lower = name.to_lowercase();
    if lower == "inbox" {
        Some("inbox")
    } else if lower == "sent" || lower == "sent mail" || lower == "sent messages" {
        Some("sent")
    } else if lower == "drafts" || lower == "draft" {
        Some("drafts")
    } else if lower == "trash" || lower == "deleted" || lower == "deleted messages" {
        Some("trash")
    } else if lower == "spam" || lower == "junk" || lower == "bulk mail" {
        Some("spam")
    } else if lower == "archive" || lower == "all mail" || lower == "all messages" {
        Some("archive")
    } else {
        None
    }
}

// ── Sync cursor ─────────────────────────────────────────────────────

/// Stored sync cursor for a folder.
pub(crate) struct SyncCursorInfo {
    pub(crate) uid_validity: u32,
    pub(crate) last_uid: u32,
}

/// Load the sync cursor for a folder.
pub(crate) async fn load_cursor(
    db: &DbPool,
    account_id: &str,
    folder_id: &str,
) -> Result<Option<SyncCursorInfo>, SyncError> {
    let value: Option<String> = db_scalar_optional!(
        db,
        String,
        r"
        SELECT cursor_value
        FROM sync_cursor
        WHERE account_id = ? AND folder_id = ? AND cursor_type = 'uidvalidity_uid'
        ",
        &id_param(db, account_id)?,
        &id_param(db, folder_id)?
    )?;
    Ok(value.as_deref().map(parse_cursor_value))
}

/// Save the sync cursor for a folder.
///
/// Cursor format: `{uid_validity}:{last_uid}` for the `uidvalidity_uid` type.
#[cfg(test)]
pub(crate) async fn save_cursor(
    db: &DbPool,
    account_id: &str,
    folder_id: &str,
    protocol: &str,
    uid_validity: u32,
    last_uid: u32,
) -> Result<(), SyncError> {
    let mut tx = db.begin().await?;
    save_cursor_in_tx(
        &mut tx,
        db,
        account_id,
        folder_id,
        protocol,
        uid_validity,
        last_uid,
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Parse a cursor value string `{uid_validity}:{last_uid}`.
pub(crate) fn parse_cursor_value(value: &str) -> SyncCursorInfo {
    let parts: Vec<&str> = value.splitn(2, ':').collect();
    if parts.len() == 2 {
        SyncCursorInfo {
            uid_validity: parts[0].parse().unwrap_or(0),
            last_uid: parts[1].parse().unwrap_or(0),
        }
    } else {
        SyncCursorInfo {
            uid_validity: 0,
            last_uid: 0,
        }
    }
}

/// Load the stored JMAP `queryState` token for a folder.
///
/// Returns the raw token to be sent verbatim as `sinceQueryState` on the next sync.
pub(crate) async fn load_jmap_cursor(
    db: &DbPool,
    account_id: &str,
    folder_id: &str,
) -> Result<Option<String>, SyncError> {
    db_scalar_optional!(
        db,
        String,
        r"
        SELECT cursor_value
        FROM sync_cursor
        WHERE account_id = ? AND folder_id = ? AND cursor_type = 'state_token'
        ",
        &id_param(db, account_id)?,
        &id_param(db, folder_id)?
    )
    .map_err(SyncError::from)
}

/// Save the JMAP `queryState` token for a folder.
///
/// The token is an opaque server string and is stored verbatim — never hashed —
/// so it can be sent back as `sinceQueryState` on the next sync.
#[cfg(test)]
pub(crate) async fn save_jmap_cursor(
    db: &DbPool,
    account_id: &str,
    folder_id: &str,
    query_state: &str,
) -> Result<(), SyncError> {
    let mut tx = db.begin().await?;
    save_jmap_cursor_in_tx(&mut tx, db, account_id, folder_id, query_state).await?;
    tx.commit().await?;
    Ok(())
}

pub(crate) async fn clear_jmap_cursor(
    db: &DbPool,
    account_id: &str,
    folder_id: &str,
) -> Result<(), SyncError> {
    db_execute!(
        db,
        r"
        DELETE FROM sync_cursor
        WHERE account_id = ? AND folder_id = ? AND cursor_type = 'state_token'
        ",
        &id_param(db, account_id)?,
        &id_param(db, folder_id)?
    )?;
    Ok(())
}

// ── Message upsert ──────────────────────────────────────────────────

/// Upsert a message from IMAP metadata.
///
/// Returns `true` if the message was newly inserted, `false` if updated.
#[cfg(test)]
pub(crate) async fn upsert_message(
    db: &DbPool,
    account_id: &str,
    folder_id: &str,
    msg: &ImapMessage,
) -> Result<bool, SyncError> {
    let mut tx = db.begin().await?;
    let was_new = upsert_message_in_tx(&mut tx, db, account_id, folder_id, msg).await?;
    tx.commit().await?;
    Ok(was_new)
}

/// Delete all messages in a folder (used when UIDVALIDITY changes).
#[cfg(test)]
pub(crate) async fn clear_folder_messages(db: &DbPool, folder_id: &str) -> Result<(), SyncError> {
    let mut tx = db.begin().await?;
    clear_folder_messages_in_tx(&mut tx, db, folder_id).await?;
    tx.commit().await?;
    Ok(())
}

/// Update folder message counts from the message table.
pub(crate) async fn update_folder_counts(db: &DbPool, folder_id: &str) -> Result<(), SyncError> {
    let mut tx = db.begin().await?;
    update_folder_counts_in_tx(&mut tx, db, folder_id).await?;
    tx.commit().await?;
    Ok(())
}

/// Persist one IMAP folder page: optional wipe, upserts, cursor, counts — one transaction.
///
/// Returns `(messages_new, messages_updated)`.
pub(crate) async fn persist_imap_folder_batch(
    db: &DbPool,
    account_id: &str,
    folder_id: &str,
    messages: &[ImapMessage],
    uid_validity: u32,
    last_uid: u32,
    clear_first: bool,
) -> Result<(usize, usize), SyncError> {
    let mut tx = db.begin().await?;
    if clear_first {
        clear_folder_messages_in_tx(&mut tx, db, folder_id).await?;
    }
    let mut new = 0usize;
    let mut updated = 0usize;
    for msg in messages {
        if upsert_message_in_tx(&mut tx, db, account_id, folder_id, msg).await? {
            new += 1;
        } else {
            updated += 1;
        }
    }
    save_cursor_in_tx(
        &mut tx,
        db,
        account_id,
        folder_id,
        "imap",
        uid_validity,
        last_uid,
    )
    .await?;
    update_folder_counts_in_tx(&mut tx, db, folder_id).await?;
    tx.commit().await?;
    Ok((new, updated))
}

/// Persist one JMAP mailbox page: upserts, cursor, counts — one transaction.
pub(crate) async fn persist_jmap_folder_batch(
    db: &DbPool,
    account_id: &str,
    folder_id: &str,
    emails: &[crate::jmap::JmapEmail],
    query_state: Option<&str>,
) -> Result<(usize, usize), SyncError> {
    let mut tx = db.begin().await?;
    let mut new = 0usize;
    let mut updated = 0usize;
    for email in emails {
        if upsert_jmap_message_in_tx(&mut tx, db, account_id, folder_id, email).await? {
            new += 1;
        } else {
            updated += 1;
        }
    }
    if let Some(qs) = query_state {
        save_jmap_cursor_in_tx(&mut tx, db, account_id, folder_id, qs).await?;
    }
    update_folder_counts_in_tx(&mut tx, db, folder_id).await?;
    tx.commit().await?;
    Ok((new, updated))
}

pub(crate) async fn upsert_message_in_tx(
    tx: &mut DbTxn,
    db: &DbPool,
    account_id: &str,
    folder_id: &str,
    msg: &ImapMessage,
) -> Result<bool, SyncError> {
    let external_id = msg.uid.to_string();
    let account_bind = id_param(db, account_id)?;
    let folder_bind = id_param(db, folder_id)?;

    let existing: Option<String> = db_txn_id_optional!(
        tx,
        "SELECT id FROM message WHERE account_id = ? AND external_id = ?",
        &account_bind,
        &external_id
    )?;

    let flags_json = serde_json::to_string(&msg.flags).unwrap_or_else(|_| "{}".into());
    let is_read = msg
        .flags
        .iter()
        .any(|f| f.contains("Seen") || f.contains("\\Seen"));
    let is_starred = msg
        .flags
        .iter()
        .any(|f| f.contains("Flagged") || f.contains("\\Flagged"));
    let snippet = msg.subject.as_deref().map(|s| {
        if s.len() > 120 {
            format!("{}...", &s[..117])
        } else {
            s.to_string()
        }
    });

    let from_json = msg
        .from
        .as_ref()
        .map(|f| serde_json::json!({ "raw": f }).to_string());
    let to_json = msg
        .to
        .as_ref()
        .map(|t| serde_json::json!(vec![t]).to_string());

    let was_new = existing.is_none();
    let id =
        existing.unwrap_or_else(|| Uuid::new_v7(uuid::Timestamp::now(uuid::NoContext)).to_string());

    db_txn_execute!(
        tx,
        r"
        INSERT INTO message (
            id, account_id, folder_id, external_id,
            message_id_header, subject, from_address, to_addresses,
            cc_addresses, date, is_read, is_starred,
            flags, size_bytes, in_reply_to, references_headers,
            snippet, has_attachments, body_text, body_html, snoozed_until
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, NULL)
        ON CONFLICT(account_id, external_id) DO UPDATE SET
            is_read = excluded.is_read,
            is_starred = excluded.is_starred,
            flags = excluded.flags,
            subject = COALESCE(NULLIF(subject, ''), excluded.subject),
            from_address = COALESCE(from_address, excluded.from_address),
            to_addresses = COALESCE(to_addresses, excluded.to_addresses),
            cc_addresses = COALESCE(cc_addresses, excluded.cc_addresses),
            date = COALESCE(date, excluded.date),
            snippet = COALESCE(NULLIF(snippet, ''), excluded.snippet),
            message_id_header = COALESCE(message_id_header, excluded.message_id_header),
            updated_at = datetime('now')
        ",
        &id_param(db, &id)?,
        &account_bind,
        &folder_bind,
        &external_id,
        &msg.message_id,
        &msg.subject,
        opt_json_param(db, from_json.as_deref()),
        opt_json_param(db, to_json.as_deref()),
        opt_json_param(db, msg.cc.as_deref()),
        message_date_param(db, msg.date.as_deref()),
        is_read,
        is_starred,
        opt_json_param(db, Some(flags_json.as_str())),
        msg.size.map(i32::try_from).transpose().unwrap_or(None),
        &msg.in_reply_to,
        &msg.references,
        &snippet,
        msg.has_attachments,
        &msg.body_text,
        persist_body_html(msg.body_html.as_deref())
    )?;

    Ok(was_new)
}

pub(crate) async fn upsert_jmap_message_in_tx(
    tx: &mut DbTxn,
    db: &DbPool,
    account_id: &str,
    folder_id: &str,
    email: &crate::jmap::JmapEmail,
) -> Result<bool, SyncError> {
    let external_id = &email.id;
    let account_bind = id_param(db, account_id)?;
    let folder_bind = id_param(db, folder_id)?;

    let existing: Option<String> = db_txn_id_optional!(
        tx,
        "SELECT id FROM message WHERE account_id = ? AND external_id = ?",
        &account_bind,
        external_id
    )?;

    let is_read = email.is_seen();
    let is_starred = email.is_flagged();
    let snippet = email.preview.clone().or_else(|| {
        email.subject.as_ref().map(|s| {
            if s.len() > 120 {
                format!("{}...", &s[..117])
            } else {
                s.clone()
            }
        })
    });

    let from_json = email
        .format_from()
        .map(|f| serde_json::json!({ "raw": f }).to_string());
    let to_json = email
        .to_string_list()
        .map(|t| serde_json::json!(vec![t]).to_string());
    let cc_json = email.cc.as_ref().map(|addrs| {
        let formatted: Vec<String> = addrs
            .iter()
            .map(|a| match (&a.name, &a.email) {
                (Some(name), Some(email)) => format!("{name} <{email}>"),
                (None, Some(email)) => email.clone(),
                _ => String::new(),
            })
            .collect();
        serde_json::json!(formatted).to_string()
    });

    let flags_json = serde_json::to_string(&email.keywords).unwrap_or_else(|_| "{}".into());

    if let Some(id) = existing {
        db_txn_execute!(
            tx,
            r"
            UPDATE message SET
                is_read = ?,
                is_starred = ?,
                flags = ?,
                updated_at = datetime('now')
            WHERE id = ?
            ",
            is_read,
            is_starred,
            &opt_json_param(db, Some(flags_json.as_str())),
            &id_param(db, &id)?
        )?;
        Ok(false)
    } else {
        let id = uuid::Uuid::new_v7(uuid::Timestamp::now(uuid::NoContext)).to_string();
        db_txn_execute!(
            tx,
            r"
            INSERT INTO message (
                id, account_id, folder_id, external_id,
                message_id_header, subject, from_address, to_addresses,
                cc_addresses, date, is_read, is_starred,
                flags, size_bytes, in_reply_to, references_headers,
                snippet, has_attachments, body_text, body_html
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            ",
            &id_param(db, &id)?,
            &account_bind,
            &folder_bind,
            external_id,
            email.message_id_header(),
            &email.subject,
            opt_json_param(db, from_json.as_deref()),
            opt_json_param(db, to_json.as_deref()),
            opt_json_param(db, cc_json.as_deref()),
            message_date_param(db, email.received_at.as_deref()),
            is_read,
            is_starred,
            opt_json_param(db, Some(flags_json.as_str())),
            email.size.map(|s| i32::try_from(s).unwrap_or(i32::MAX)),
            email
                .in_reply_to
                .as_ref()
                .and_then(|ids| ids.first())
                .cloned(),
            email.references.as_ref().map(|refs| refs.join(" ")),
            &snippet,
            email.has_attachment.unwrap_or(false),
            email.body_text(),
            persist_body_html(email.body_html().as_deref())
        )?;
        Ok(true)
    }
}

pub(crate) async fn save_cursor_in_tx(
    tx: &mut DbTxn,
    db: &DbPool,
    account_id: &str,
    folder_id: &str,
    protocol: &str,
    uid_validity: u32,
    last_uid: u32,
) -> Result<(), SyncError> {
    let cursor_value = format!("{uid_validity}:{last_uid}");
    let cursor_id = Uuid::new_v7(uuid::Timestamp::now(uuid::NoContext)).to_string();
    db_txn_execute!(
        tx,
        r"
        INSERT INTO sync_cursor (id, account_id, folder_id, protocol, cursor_type, cursor_value, updated_at)
        VALUES (?, ?, ?, ?, 'uidvalidity_uid', ?, datetime('now'))
        ON CONFLICT(account_id, folder_id, cursor_type)
        DO UPDATE SET cursor_value = excluded.cursor_value, updated_at = excluded.updated_at
        ",
        &id_param(db, &cursor_id)?,
        &id_param(db, account_id)?,
        &id_param(db, folder_id)?,
        protocol,
        &cursor_value
    )?;
    Ok(())
}

pub(crate) async fn save_jmap_cursor_in_tx(
    tx: &mut DbTxn,
    db: &DbPool,
    account_id: &str,
    folder_id: &str,
    query_state: &str,
) -> Result<(), SyncError> {
    let cursor_id = Uuid::new_v7(uuid::Timestamp::now(uuid::NoContext)).to_string();
    db_txn_execute!(
        tx,
        r"
        INSERT INTO sync_cursor (id, account_id, folder_id, protocol, cursor_type, cursor_value, updated_at)
        VALUES (?, ?, ?, 'jmap', 'state_token', ?, datetime('now'))
        ON CONFLICT(account_id, folder_id, cursor_type)
        DO UPDATE SET cursor_value = excluded.cursor_value, updated_at = excluded.updated_at
        ",
        &id_param(db, &cursor_id)?,
        &id_param(db, account_id)?,
        &id_param(db, folder_id)?,
        query_state
    )?;
    Ok(())
}

pub(crate) async fn clear_folder_messages_in_tx(
    tx: &mut DbTxn,
    db: &DbPool,
    folder_id: &str,
) -> Result<(), SyncError> {
    let folder_bind = id_param(db, folder_id)?;
    db_txn_execute!(tx, "DELETE FROM message WHERE folder_id = ?", &folder_bind)?;
    db_txn_execute!(
        tx,
        "DELETE FROM sync_cursor WHERE folder_id = ?",
        &folder_bind
    )?;
    Ok(())
}

pub(crate) async fn update_folder_counts_in_tx(
    tx: &mut DbTxn,
    db: &DbPool,
    folder_id: &str,
) -> Result<(), SyncError> {
    let folder_bind = id_param(db, folder_id)?;
    let total: i64 = db_txn_scalar!(
        tx,
        i64,
        "SELECT COUNT(*) FROM message WHERE folder_id = ? AND is_deleted = ?",
        &folder_bind,
        false
    )?;
    let unread: i64 = db_txn_scalar!(
        tx,
        i64,
        "SELECT COUNT(*) FROM message WHERE folder_id = ? AND is_deleted = ? AND is_read = ?",
        &folder_bind,
        false,
        false
    )?;
    db_txn_execute!(
        tx,
        "UPDATE folder SET total_messages = ?, unread_messages = ?, updated_at = datetime('now') WHERE id = ?",
        i32::try_from(total).unwrap_or(i32::MAX),
        i32::try_from(unread).unwrap_or(i32::MAX),
        &folder_bind
    )?;
    Ok(())
}
