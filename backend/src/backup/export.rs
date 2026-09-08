//! Export collectors: snapshot every section into a staging dir.
//! Spec: docs/superpowers/specs/2026-09-08-lyra-backup-export-import-design.md §3-§5.
//!
//! Layout written under `staging/`:
//! `settings.json`, `accounts/<n>.json`, `contacts.vcf`, `calendars/<n>.ics` +
//! `calendars.json`, `blobs/<sha256>`. Mail (`mail/<n>/…`) is collected by
//! [`collect_mail`]. Decrypted credentials and raw message bytes are never
//! logged.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
use sea_orm::sea_query::{Expr, Order, Query as Sq};
use sea_orm::{ColumnTrait, ConnectionTrait, ExprTrait, QueryResult, Value};
use serde_json::{Value as Json, json};
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

use crate::auth::AuthState;
use crate::blobs;
use crate::crypto::{self, EncryptedCredential};
use crate::db_row::{IdParam, id_param, parse_ts};
use crate::entities::{
    attachment, calendar, calendar_event, contact, folder, lyra_user, mail_account, message,
};
use crate::kv::KvStore;
use crate::storage::DbPool;
use crate::sync::store;

use super::BackupError;
use super::format::{MetaLine, write_mbox_message};

/// Section counts for the manifest plus non-fatal warnings (missing blobs).
#[derive(Debug, Default)]
pub(crate) struct ExportCounts {
    pub accounts: u32,
    pub messages: u64,
    pub contacts: u32,
    pub calendars: u32,
    pub blobs: u64,
    pub warnings: Vec<String>,
}

/// Fresh staging dir `<data_dir>/backups/staging/<uuid>/` (0700 on unix).
pub(crate) async fn create_staging_dir(data_dir: &Path) -> Result<PathBuf, BackupError> {
    let dir = data_dir
        .join("backups")
        .join("staging")
        .join(Uuid::new_v7(uuid::Timestamp::now(uuid::NoContext)).to_string());
    tokio::fs::create_dir_all(&dir).await?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).await?;
    }
    Ok(dir)
}

/// Collect the non-mail sections into `staging`. Returns counts for the manifest.
pub(crate) async fn collect(
    state: &AuthState,
    user_id: &str,
    staging: &Path,
) -> Result<ExportCounts, BackupError> {
    let db = &state.db;
    collect_settings(db, user_id, staging).await?;

    let accounts = load_accounts(db, user_id).await?;
    let mut counts = ExportCounts {
        accounts: u32::try_from(accounts.len()).unwrap_or(u32::MAX),
        ..ExportCounts::default()
    };

    if !accounts.is_empty() {
        let dek = AuthState::get_user_dek(db, user_id)
            .await
            .map_err(|e| BackupError::Crypto(e.to_string()))?;
        tokio::fs::create_dir_all(staging.join("accounts")).await?;
        for (index, account) in accounts.iter().enumerate() {
            collect_account(db, index, account, &dek, staging).await?;
        }
    }

    let account_ids: Vec<String> = accounts.iter().map(|a| a.id.clone()).collect();
    let (contacts, photo_paths) = collect_contacts(db, &account_ids, staging).await?;
    counts.contacts = contacts;
    counts.calendars = collect_calendars(db, &account_ids, staging).await?;
    let (blobs_copied, warnings) =
        collect_blobs(db, &account_ids, &state.data_dir, staging, photo_paths).await?;
    counts.blobs = blobs_copied;
    counts.warnings = warnings;
    Ok(counts)
}

// ── DB row decoding (dialect-aware: TEXT ids/timestamps on SQLite+MySQL) ──

/// Recover the driver error so `BackupError::Db` reports the sqlx failure.
fn orm_err(err: sea_orm::DbErr) -> BackupError {
    use sea_orm::RuntimeErr;
    let sqlx_err = match err {
        sea_orm::DbErr::Exec(RuntimeErr::SqlxError(e))
        | sea_orm::DbErr::Query(RuntimeErr::SqlxError(e))
        | sea_orm::DbErr::Conn(RuntimeErr::SqlxError(e)) => std::sync::Arc::try_unwrap(e)
            .unwrap_or_else(|shared| sqlx::Error::Protocol(shared.to_string())),
        other => sqlx::Error::Protocol(other.to_string()),
    };
    BackupError::Db(sqlx_err)
}

/// Map a sync-store error (raw-blob path read/write) into BackupError.
fn sync_err(err: crate::sync::SyncError) -> BackupError {
    match err {
        crate::sync::SyncError::Database(e) => BackupError::Db(e),
        other => BackupError::Internal(other.to_string()),
    }
}

/// UUID-column id bind: TEXT on SQLite/MySQL, native `Uuid` on Postgres.
fn id_bind(db: &DbPool, id: &str) -> Result<Value, BackupError> {
    let param = id_param(db, id).map_err(|e| BackupError::Internal(e.to_string()))?;
    Ok(match param {
        IdParam::Text(s) => Value::String(Some(s)),
        IdParam::Uuid(u) => Value::Uuid(Some(u)),
    })
}

/// UUID/TEXT id column: `String` on SQLite/MySQL, native UUID on Postgres.
fn row_id(row: &QueryResult, col: &str) -> Result<String, BackupError> {
    if let Some(text) = row.try_get::<Option<String>>("", col).ok().flatten() {
        return Ok(text);
    }
    row.try_get::<Option<Uuid>>("", col)
        .map_err(orm_err)?
        .map(|u| u.to_string())
        .ok_or_else(|| BackupError::Internal(format!("missing column {col}")))
}

/// Nullable UUID/TEXT id column ([`row_id`] semantics).
fn row_opt_id(row: &QueryResult, col: &str) -> Result<Option<String>, BackupError> {
    if let Ok(text) = row.try_get::<Option<String>>("", col) {
        return Ok(text);
    }
    Ok(row
        .try_get::<Option<Uuid>>("", col)
        .map_err(orm_err)?
        .map(|u| u.to_string()))
}

/// Nullable timestamp column: TEXT on SQLite/MySQL, native on Postgres;
/// normalized to an RFC3339 string for downstream `parse_ts`.
fn row_opt_ts(row: &QueryResult, col: &str) -> Result<Option<String>, BackupError> {
    if let Ok(text) = row.try_get::<Option<String>>("", col) {
        return Ok(text);
    }
    row.try_get::<Option<chrono::DateTime<chrono::Utc>>>("", col)
        .map(|opt| opt.map(|t| t.to_rfc3339()))
        .map_err(orm_err)
}

/// JSONB / TEXT json column → JSON text (`from_address`, …).
fn row_json_text(row: &QueryResult, col: &str) -> Result<Option<String>, BackupError> {
    if let Ok(text) = row.try_get::<Option<String>>("", col) {
        return Ok(text);
    }
    let value: Option<Json> = row.try_get("", col).map_err(orm_err)?;
    Ok(value.filter(|v| !v.is_null()).map(|v| v.to_string()))
}

fn row_opt_str(row: &QueryResult, col: &str) -> Result<Option<String>, BackupError> {
    row.try_get::<Option<String>>("", col).map_err(orm_err)
}

fn row_opt_i32(row: &QueryResult, col: &str) -> Result<Option<i32>, BackupError> {
    row.try_get::<Option<i32>>("", col).map_err(orm_err)
}

fn row_bool(row: &QueryResult, col: &str) -> Result<bool, BackupError> {
    row.try_get::<bool>("", col).map_err(orm_err)
}

async fn write_json(path: PathBuf, value: &Json) -> Result<(), BackupError> {
    let bytes = serde_json::to_vec_pretty(value)?;
    Ok(tokio::fs::write(path, bytes).await?)
}

// ── Accounts ──────────────────────────────────────────────────────────

pub(crate) struct AccountRow {
    pub id: String,
    pub display_name: Option<String>,
    pub email_address: String,
    pub protocol: String,
    pub auth_type: String,
    pub credential: String,
    pub imap_host: Option<String>,
    pub imap_port: Option<i32>,
    pub imap_security: Option<String>,
    pub jmap_base_url: Option<String>,
    pub smtp_host: Option<String>,
    pub smtp_port: Option<i32>,
    pub smtp_security: Option<String>,
    pub smtp_auth_type: Option<String>,
    pub smtp_credential: Option<String>,
    pub pim_credential: Option<String>,
    pub signature: Option<String>,
    pub carddav_url: Option<String>,
    pub caldav_url: Option<String>,
    pub sync_enabled: bool,
    pub receive_protocol: String,
    pub send_protocol: String,
}

/// Accounts of the user ordered by `created_at` — the `<n>` index used for
/// `accounts/<n>.json` and `mail/<n>/` across the whole archive.
pub(crate) async fn load_accounts(
    db: &DbPool,
    user_id: &str,
) -> Result<Vec<AccountRow>, BackupError> {
    let mut sel = Sq::select();
    sel.columns([
        mail_account::Column::Id,
        mail_account::Column::DisplayName,
        mail_account::Column::EmailAddress,
        mail_account::Column::Protocol,
        mail_account::Column::AuthType,
        mail_account::Column::Credential,
        mail_account::Column::ImapHost,
        mail_account::Column::ImapPort,
        mail_account::Column::ImapSecurity,
        mail_account::Column::JmapBaseUrl,
        mail_account::Column::SmtpHost,
        mail_account::Column::SmtpPort,
        mail_account::Column::SmtpSecurity,
        mail_account::Column::SmtpAuthType,
        mail_account::Column::SmtpCredential,
        mail_account::Column::PimCredential,
        mail_account::Column::Signature,
        mail_account::Column::CarddavUrl,
        mail_account::Column::CaldavUrl,
        mail_account::Column::SyncEnabled,
        mail_account::Column::ReceiveProtocol,
        mail_account::Column::SendProtocol,
    ])
    .from(mail_account::Entity)
    .and_where(mail_account::Column::UserId.eq(id_bind(db, user_id)?))
    .order_by_expr(Expr::col(mail_account::Column::CreatedAt), Order::Asc);
    let rows = db.orm().query_all(&sel).await.map_err(orm_err)?;
    rows.iter().map(account_from_row).collect()
}

fn account_from_row(row: &QueryResult) -> Result<AccountRow, BackupError> {
    Ok(AccountRow {
        id: row_id(row, "id")?,
        display_name: row_opt_str(row, "display_name")?,
        email_address: row
            .try_get::<String>("", "email_address")
            .map_err(orm_err)?,
        protocol: row.try_get::<String>("", "protocol").map_err(orm_err)?,
        auth_type: row.try_get::<String>("", "auth_type").map_err(orm_err)?,
        credential: row.try_get::<String>("", "credential").map_err(orm_err)?,
        imap_host: row_opt_str(row, "imap_host")?,
        imap_port: row_opt_i32(row, "imap_port")?,
        imap_security: row_opt_str(row, "imap_security")?,
        jmap_base_url: row_opt_str(row, "jmap_base_url")?,
        smtp_host: row_opt_str(row, "smtp_host")?,
        smtp_port: row_opt_i32(row, "smtp_port")?,
        smtp_security: row_opt_str(row, "smtp_security")?,
        smtp_auth_type: row_opt_str(row, "smtp_auth_type")?,
        smtp_credential: row_opt_str(row, "smtp_credential")?,
        pim_credential: row_opt_str(row, "pim_credential")?,
        signature: row_opt_str(row, "signature")?,
        carddav_url: row_opt_str(row, "carddav_url")?,
        caldav_url: row_opt_str(row, "caldav_url")?,
        sync_enabled: row_bool(row, "sync_enabled")?,
        receive_protocol: row
            .try_get::<String>("", "receive_protocol")
            .map_err(orm_err)?,
        send_protocol: row
            .try_get::<String>("", "send_protocol")
            .map_err(orm_err)?,
    })
}

pub(crate) struct FolderRow {
    pub id: String,
    pub external_id: Option<String>,
    pub name: String,
    pub parent_id: Option<String>,
    pub role: Option<String>,
    pub role_override: Option<String>,
    pub sort_order: i32,
}

/// Folders of one account in display order.
pub(crate) async fn load_account_folders(
    db: &DbPool,
    account_id: &str,
) -> Result<Vec<FolderRow>, BackupError> {
    let mut sel = Sq::select();
    sel.columns([
        folder::Column::Id,
        folder::Column::ExternalId,
        folder::Column::Name,
        folder::Column::ParentId,
        folder::Column::Role,
        folder::Column::RoleOverride,
        folder::Column::SortOrder,
    ])
    .from(folder::Entity)
    .and_where(folder::Column::AccountId.eq(id_bind(db, account_id)?))
    .order_by_expr(Expr::col(folder::Column::SortOrder), Order::Asc)
    .order_by_expr(Expr::col(folder::Column::CreatedAt), Order::Asc);
    let rows = db.orm().query_all(&sel).await.map_err(orm_err)?;
    rows.iter()
        .map(|row| {
            Ok(FolderRow {
                id: row_id(row, "id")?,
                external_id: row_opt_str(row, "external_id")?,
                name: row.try_get::<String>("", "name").map_err(orm_err)?,
                parent_id: row_opt_id(row, "parent_id")?,
                role: row_opt_str(row, "role")?,
                role_override: row_opt_str(row, "role_override")?,
                sort_order: row.try_get::<i32>("", "sort_order").map_err(orm_err)?,
            })
        })
        .collect()
}

/// Decrypt a credential column to its inner JSON value (UTF-8 JSON), a plain
/// string (non-JSON UTF-8), or base64 (binary). NULL column → JSON null.
/// The plaintext is never logged.
fn decrypt_credential(dek: &[u8], column: Option<&str>) -> Result<Json, BackupError> {
    let Some(raw) = column else {
        return Ok(Json::Null);
    };
    let envelope: EncryptedCredential = serde_json::from_str(raw)?;
    let plain = crypto::decrypt(dek, &envelope).map_err(|e| BackupError::Crypto(e.to_string()))?;
    Ok(match std::str::from_utf8(&plain) {
        Ok(text) => serde_json::from_str(text).unwrap_or_else(|_| Json::String(text.to_string())),
        Err(_) => Json::String(B64.encode(&plain)),
    })
}

/// `settings.json` — the user's UI state blob.
async fn collect_settings(db: &DbPool, user_id: &str, staging: &Path) -> Result<(), BackupError> {
    let mut sel = Sq::select();
    sel.column(lyra_user::Column::UiState)
        .from(lyra_user::Entity)
        .and_where(lyra_user::Column::Id.eq(id_bind(db, user_id)?));
    let row = db.orm().query_one(&sel).await.map_err(orm_err)?;
    let raw: Option<String> = match row {
        Some(r) => r.try_get("", "ui_state").map_err(orm_err)?,
        None => None,
    };
    let ui_state: Json = raw
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(Json::Null);
    write_json(
        staging.join("settings.json"),
        &json!({ "ui_state": ui_state }),
    )
    .await
}

/// `accounts/<index>.json` — account config with decrypted credentials and
/// the folder list inline (parent linkage by `external_id`).
async fn collect_account(
    db: &DbPool,
    index: usize,
    account: &AccountRow,
    dek: &[u8],
    staging: &Path,
) -> Result<(), BackupError> {
    let folders = load_account_folders(db, &account.id).await?;
    let external_by_id: std::collections::HashMap<&str, Option<&str>> = folders
        .iter()
        .map(|f| (f.id.as_str(), f.external_id.as_deref()))
        .collect();
    let folders_json: Vec<Json> = folders
        .iter()
        .map(|f| {
            let parent_external_id = f
                .parent_id
                .as_deref()
                .and_then(|pid| external_by_id.get(pid).copied().flatten());
            json!({
                "id": f.id,
                "external_id": f.external_id,
                "name": f.name,
                "parent_external_id": parent_external_id,
                "role": f.role,
                "role_override": f.role_override,
                "sort_order": f.sort_order,
            })
        })
        .collect();
    let doc = json!({
        "index": index,
        "display_name": account.display_name,
        "email_address": account.email_address,
        "protocol": account.protocol,
        "auth_type": account.auth_type,
        "imap_host": account.imap_host,
        "imap_port": account.imap_port,
        "imap_security": account.imap_security,
        "jmap_base_url": account.jmap_base_url,
        "smtp_host": account.smtp_host,
        "smtp_port": account.smtp_port,
        "smtp_security": account.smtp_security,
        "smtp_auth_type": account.smtp_auth_type,
        "signature": account.signature,
        "carddav_url": account.carddav_url,
        "caldav_url": account.caldav_url,
        "sync_enabled": account.sync_enabled,
        "receive_protocol": account.receive_protocol,
        "send_protocol": account.send_protocol,
        "credential": decrypt_credential(dek, Some(&account.credential))?,
        "smtp_credential": decrypt_credential(dek, account.smtp_credential.as_deref())?,
        "pim_credential": decrypt_credential(dek, account.pim_credential.as_deref())?,
        "folders": folders_json,
    });
    write_json(staging.join("accounts").join(format!("{index}.json")), &doc).await
}

/// `contacts.vcf` — every contact's inline vCard (`vcard_blob` holds the
/// vCard text itself, written by `pim_dav`, not a blob-store path), separated
/// by CRLF. Also returns `photo_path`s for the blob collector.
async fn collect_contacts(
    db: &DbPool,
    account_ids: &[String],
    staging: &Path,
) -> Result<(u32, Vec<String>), BackupError> {
    let mut out = String::new();
    let mut count = 0u32;
    let mut photo_paths = Vec::new();
    if !account_ids.is_empty() {
        let binds = account_ids
            .iter()
            .map(|id| id_bind(db, id))
            .collect::<Result<Vec<_>, _>>()?;
        let mut sel = Sq::select();
        sel.column(contact::Column::VcardBlob)
            .column(contact::Column::PhotoPath)
            .from(contact::Entity)
            .and_where(contact::Column::AccountId.is_in(binds))
            .order_by_expr(Expr::col(contact::Column::CreatedAt), Order::Asc);
        let rows = db.orm().query_all(&sel).await.map_err(orm_err)?;
        for row in &rows {
            if let Some(vcard) = row_opt_str(row, "vcard_blob")?.filter(|v| !v.trim().is_empty()) {
                out.push_str(vcard.trim_end_matches(['\r', '\n']));
                out.push_str("\r\n");
                count += 1;
            }
            if let Some(path) = row_opt_str(row, "photo_path")? {
                photo_paths.push(path);
            }
        }
    }
    tokio::fs::write(staging.join("contacts.vcf"), out).await?;
    Ok((count, photo_paths))
}

/// `calendars/<n>.ics` per calendar row plus the root `calendars.json` index.
/// Event `icalendar_blob` is inline iCal text (VEVENT), written by `pim_dav`.
async fn collect_calendars(
    db: &DbPool,
    account_ids: &[String],
    staging: &Path,
) -> Result<u32, BackupError> {
    let mut index_json: Vec<Json> = Vec::new();
    if !account_ids.is_empty() {
        let binds = account_ids
            .iter()
            .map(|id| id_bind(db, id))
            .collect::<Result<Vec<_>, _>>()?;
        let mut sel = Sq::select();
        sel.columns([
            calendar::Column::Id,
            calendar::Column::Name,
            calendar::Column::Color,
            calendar::Column::Description,
            calendar::Column::Timezone,
        ])
        .from(calendar::Entity)
        .and_where(calendar::Column::AccountId.is_in(binds))
        .order_by_expr(Expr::col(calendar::Column::CreatedAt), Order::Asc);
        let rows = db.orm().query_all(&sel).await.map_err(orm_err)?;
        if !rows.is_empty() {
            tokio::fs::create_dir_all(staging.join("calendars")).await?;
        }
        for (index, row) in rows.iter().enumerate() {
            let id = row_id(row, "id")?;
            let name: String = row.try_get("", "name").map_err(orm_err)?;
            let mut ics = String::from("BEGIN:VCALENDAR\r\nVERSION:2.0\r\n");
            let mut esel = Sq::select();
            esel.column(calendar_event::Column::IcalendarBlob)
                .from(calendar_event::Entity)
                .and_where(calendar_event::Column::CalendarId.eq(id_bind(db, &id)?))
                .order_by_expr(Expr::col(calendar_event::Column::CreatedAt), Order::Asc);
            let events = db.orm().query_all(&esel).await.map_err(orm_err)?;
            for event in &events {
                if let Some(blob) =
                    row_opt_str(event, "icalendar_blob")?.filter(|b| !b.trim().is_empty())
                {
                    ics.push_str(blob.trim_end_matches(['\r', '\n']));
                    ics.push_str("\r\n");
                }
            }
            ics.push_str("END:VCALENDAR\r\n");
            tokio::fs::write(staging.join("calendars").join(format!("{index}.ics")), ics).await?;
            index_json.push(json!({
                "index": index,
                "name": name,
                "color": row_opt_str(row, "color")?,
                "description": row_opt_str(row, "description")?,
                "timezone": row_opt_str(row, "timezone")?,
            }));
        }
    }
    let count = u32::try_from(index_json.len()).unwrap_or(u32::MAX);
    write_json(staging.join("calendars.json"), &Json::Array(index_json)).await?;
    Ok(count)
}

/// `blobs/<sha256>` — attachment blobs of the user's messages plus contact
/// photos, deduped by basename (which is the content hash). Missing files
/// are warnings, never fatal.
async fn collect_blobs(
    db: &DbPool,
    account_ids: &[String],
    data_dir: &Path,
    staging: &Path,
    photo_paths: Vec<String>,
) -> Result<(u64, Vec<String>), BackupError> {
    let blobs_dir = staging.join("blobs");
    tokio::fs::create_dir_all(&blobs_dir).await?;

    let mut paths: Vec<String> = Vec::new();
    for account_id in account_ids {
        let mut sel = Sq::select();
        sel.column(attachment::Column::StoragePath)
            .from(attachment::Entity)
            .inner_join(
                message::Entity,
                Expr::col((message::Entity, message::Column::Id))
                    .equals((attachment::Entity, attachment::Column::MessageId)),
            )
            .and_where(message::Column::AccountId.eq(id_bind(db, account_id)?));
        let rows = db.orm().query_all(&sel).await.map_err(orm_err)?;
        for row in &rows {
            paths.push(row.try_get::<String>("", "storage_path").map_err(orm_err)?);
        }
    }
    paths.extend(photo_paths);

    let mut seen = HashSet::new();
    let mut copied = 0u64;
    let mut warnings = Vec::new();
    for path in paths {
        let Some(base) = Path::new(&path)
            .file_name()
            .and_then(|s| s.to_str())
            .map(str::to_owned)
        else {
            warnings.push(format!("blob path has no basename: {path}"));
            continue;
        };
        if !seen.insert(base.clone()) {
            continue;
        }
        let src = blobs::resolve_storage_path(data_dir, &path);
        match tokio::fs::copy(&src, blobs_dir.join(&base)).await {
            Ok(_) => copied += 1,
            Err(e) => warnings.push(format!("blob {base} unavailable: {e}")),
        }
    }
    Ok((copied, warnings))
}

// ── Mail (Task 6): per-folder mbox + sidecar with raw resolution ──────

/// One message row as needed for export. (`flags`/`size_bytes` columns are
/// deliberately not read: the sidecar derives flags from `is_read` /
/// `is_starred`, and mbox carries the bytes themselves.)
struct MailRow {
    id: String,
    external_id: Option<String>,
    message_id_header: Option<String>,
    subject: Option<String>,
    from_address: Option<String>,
    to_addresses: Option<String>,
    cc_addresses: Option<String>,
    date: Option<String>,
    is_read: bool,
    is_starred: bool,
    body_text: Option<String>,
    body_html: Option<String>,
}

async fn load_folder_messages(db: &DbPool, folder_id: &str) -> Result<Vec<MailRow>, BackupError> {
    let mut sel = Sq::select();
    sel.columns([
        message::Column::Id,
        message::Column::ExternalId,
        message::Column::MessageIdHeader,
        message::Column::Subject,
        message::Column::FromAddress,
        message::Column::ToAddresses,
        message::Column::CcAddresses,
        message::Column::Date,
        message::Column::IsRead,
        message::Column::IsStarred,
        message::Column::BodyText,
        message::Column::BodyHtml,
    ])
    .from(message::Entity)
    .and_where(message::Column::FolderId.eq(id_bind(db, folder_id)?))
    .order_by_expr(Expr::col(message::Column::Date), Order::Asc);
    let rows = db.orm().query_all(&sel).await.map_err(orm_err)?;
    rows.iter()
        .map(|row| {
            Ok(MailRow {
                id: row_id(row, "id")?,
                external_id: row_opt_str(row, "external_id")?,
                message_id_header: row_opt_str(row, "message_id_header")?,
                subject: row_opt_str(row, "subject")?,
                from_address: row_json_text(row, "from_address")?,
                to_addresses: row_json_text(row, "to_addresses")?,
                cc_addresses: row_json_text(row, "cc_addresses")?,
                date: row_opt_ts(row, "date")?,
                is_read: row_bool(row, "is_read")?,
                is_starred: row_bool(row, "is_starred")?,
                body_text: row_opt_str(row, "body_text")?,
                body_html: row_opt_str(row, "body_html")?,
            })
        })
        .collect()
}

/// Collect all mail into `mail/<n>/<folder-uuid>.mbox` + `.meta.jsonl` and
/// `mail/<n>/folders.json`. Returns the total messages exported.
///
/// Raw resolution order per message: stored `raw_blob_path` → batched server
/// fetch (only when `allow_server_fetch`; integration path, unreachable in
/// tests) → reconstruction from parsed columns (flagged in the sidecar).
pub(crate) async fn collect_mail(
    state: &AuthState,
    user_id: &str,
    job_id: &str,
    kv: &Arc<dyn KvStore>,
    staging: &Path,
    allow_server_fetch: bool,
) -> Result<u64, BackupError> {
    let db = &state.db;
    let accounts = load_accounts(db, user_id).await?;
    let mut total = 0u64;
    for (n, account) in accounts.iter().enumerate() {
        let dir = staging.join("mail").join(n.to_string());
        tokio::fs::create_dir_all(&dir).await?;
        let folders = load_account_folders(db, &account.id).await?;

        let mut folders_json = serde_json::Map::new();
        for f in &folders {
            folders_json.insert(
                f.id.clone(),
                json!({
                    "path": f.external_id.clone().unwrap_or_else(|| f.name.clone()),
                    "role": store::effective_folder_role(f.role.as_deref(), f.role_override.as_deref()),
                }),
            );
        }
        write_json(dir.join("folders.json"), &Json::Object(folders_json)).await?;

        for f in &folders {
            total += export_folder(state, user_id, account, f, &dir, allow_server_fetch).await?;
            let progress = json!({
                "phase": "mail",
                "account": n,
                "folder": f.external_id.clone().unwrap_or_else(|| f.name.clone()),
                "messages_done": total,
            });
            let _ = kv
                .set(
                    &format!("backup:progress:{job_id}"),
                    &progress.to_string(),
                    Some(3600),
                )
                .await;
        }
    }
    Ok(total)
}

/// Export one folder's messages; returns how many were written.
async fn export_folder(
    state: &AuthState,
    user_id: &str,
    account: &AccountRow,
    folder: &FolderRow,
    dir: &Path,
    allow_server_fetch: bool,
) -> Result<u64, BackupError> {
    let db = &state.db;
    let rows = load_folder_messages(db, &folder.id).await?;

    // Path (a): stored raw blobs.
    let mut resolved: HashMap<String, Vec<u8>> = HashMap::new();
    let mut unresolved: Vec<usize> = Vec::new();
    for (i, row) in rows.iter().enumerate() {
        let path = store::get_message_raw_blob_path(db, &row.id)
            .await
            .map_err(sync_err)?;
        let bytes = match path {
            Some(p) => blobs::read(&state.data_dir, &p).await.ok(),
            None => None,
        };
        match bytes {
            Some(b) => {
                resolved.insert(row.id.clone(), b);
            }
            None => unresolved.push(i),
        }
    }

    // Path (b): one batched server fetch per folder; failures leave the
    // message for reconstruction. Successful fetches backfill raw_blob_path.
    if allow_server_fetch && !unresolved.is_empty() {
        let refs: Vec<&MailRow> = unresolved.iter().map(|&i| &rows[i]).collect();
        let fetched =
            fetch_raw_from_server(db, user_id, account, folder.external_id.as_deref(), &refs).await;
        for (row_id, bytes) in fetched {
            if let Ok(rel) = blobs::store(&state.data_dir, &account.id, &bytes).await {
                let _ = store::set_message_raw_blob(db, &row_id, &rel).await;
            }
            resolved.insert(row_id, bytes);
        }
    }

    let mut mbox = tokio::fs::File::create(dir.join(format!("{}.mbox", folder.id))).await?;
    let mut meta = String::new();
    for row in &rows {
        // Path (c): reconstruct from parsed columns.
        let (raw, reconstructed) = match resolved.get(&row.id) {
            Some(b) => (b.clone(), false),
            None => (reconstruct_message(row), true),
        };
        let from_addr = mbox_from_addr(row.from_address.as_deref());
        let epoch = row
            .date
            .as_deref()
            .and_then(parse_ts)
            .map_or(0, |d| d.timestamp());
        let mut buf = Vec::new();
        write_mbox_message(&mut buf, &from_addr, epoch, &raw).map_err(BackupError::Io)?;
        mbox.write_all(&buf).await?;

        let mut flags = Vec::new();
        if row.is_read {
            flags.push("seen".to_string());
        }
        if row.is_starred {
            flags.push("flagged".to_string());
        }
        let line = MetaLine {
            message_id: row.message_id_header.clone(),
            flags,
            date: row
                .date
                .as_deref()
                .and_then(parse_ts)
                .map(|d| d.to_rfc3339()),
            sha256: blobs::sha256_hex(&raw),
            reconstructed,
        };
        meta.push_str(&serde_json::to_string(&line)?);
        meta.push('\n');
    }
    mbox.flush().await?;
    tokio::fs::write(dir.join(format!("{}.meta.jsonl", folder.id)), meta).await?;
    Ok(u64::try_from(rows.len()).unwrap_or(u64::MAX))
}

/// Path (b): batch-fetch raw RFC822 from the source server. Never fails the
/// export — any connection/fetch error leaves those rows for reconstruction.
async fn fetch_raw_from_server(
    db: &DbPool,
    user_id: &str,
    account: &AccountRow,
    folder_wire: Option<&str>,
    rows: &[&MailRow],
) -> HashMap<String, Vec<u8>> {
    match account.protocol.as_str() {
        "imap" => fetch_raw_imap(db, user_id, account, folder_wire, rows).await,
        "jmap" => fetch_raw_jmap(db, user_id, account, rows).await,
        _ => HashMap::new(),
    }
}

/// IMAP: connect once, select the folder, `fetch_bodies` in chunks of ≤50.
async fn fetch_raw_imap(
    db: &DbPool,
    user_id: &str,
    account: &AccountRow,
    folder_wire: Option<&str>,
    rows: &[&MailRow],
) -> HashMap<String, Vec<u8>> {
    let mut out = HashMap::new();
    let Some(wire) = folder_wire else { return out };
    let Ok((mut client, _)) =
        crate::sync::http::connect_imap_for_account(db, user_id, &account.id).await
    else {
        return out;
    };
    if client.select(wire).await.is_err() {
        return out;
    }
    let uid_rows: Vec<(u32, &str)> = rows
        .iter()
        .filter_map(|r| {
            store::parse_imap_uid(r.external_id.as_deref())
                .ok()
                .map(|uid| (uid, r.id.as_str()))
        })
        .collect();
    for chunk in uid_rows.chunks(50) {
        let uids: Vec<u32> = chunk.iter().map(|(uid, _)| *uid).collect();
        let Ok(bodies) = client.fetch_bodies(&uids).await else {
            continue;
        };
        let row_by_uid: HashMap<u32, &str> = chunk.iter().copied().collect();
        for msg in bodies {
            if let (Some(body), Some(row_id)) = (msg.body, row_by_uid.get(&msg.uid)) {
                out.insert((*row_id).to_string(), body);
            }
        }
    }
    out
}

/// JMAP: `Email/get` for blobIds (≤50 per call) + `download_blob` per id.
async fn fetch_raw_jmap(
    db: &DbPool,
    user_id: &str,
    account: &AccountRow,
    rows: &[&MailRow],
) -> HashMap<String, Vec<u8>> {
    let mut out = HashMap::new();
    let Ok(seam) = crate::sync::http::connect_jmap_for_account(db, user_id, &account.id).await
    else {
        return out;
    };
    let id_rows: Vec<(&str, &str)> = rows
        .iter()
        .filter_map(|r| r.external_id.as_deref().map(|ext| (ext, r.id.as_str())))
        .collect();
    for chunk in id_rows.chunks(50) {
        let ids: Vec<String> = chunk.iter().map(|(ext, _)| (*ext).to_string()).collect();
        let Ok((emails, _)) = seam.get_emails(&ids).await else {
            continue;
        };
        let row_by_ext: HashMap<&str, &str> = chunk.iter().copied().collect();
        for email in emails {
            let (Some(blob_id), Some(row_id)) = (email.blob_id, row_by_ext.get(email.id.as_str()))
            else {
                continue;
            };
            if let Ok(bytes) = seam.download_blob(&blob_id).await {
                out.insert((*row_id).to_string(), bytes);
            }
        }
    }
    out
}

/// Minimal RFC822 message rebuilt from parsed columns (path c). Column
/// values are already-decoded display strings; non-ASCII header text goes
/// out as RFC 2047 base64 encoded-words. HTML body wins over plain text.
fn reconstruct_message(row: &MailRow) -> Vec<u8> {
    let mut out = String::new();
    push_header(&mut out, "From", &header_from(row.from_address.as_deref()));
    if let Some(to) = header_address_list(row.to_addresses.as_deref()) {
        push_header(&mut out, "To", &to);
    }
    if let Some(cc) = header_address_list(row.cc_addresses.as_deref()) {
        push_header(&mut out, "Cc", &cc);
    }
    if let Some(subject) = row.subject.as_deref().filter(|s| !s.is_empty()) {
        push_header(&mut out, "Subject", &encode_header_value(subject));
    }
    if let Some(date) = row.date.as_deref().and_then(parse_ts) {
        push_header(&mut out, "Date", &date.to_rfc2822());
    }
    if let Some(mid) = row.message_id_header.as_deref().filter(|s| !s.is_empty()) {
        push_header(&mut out, "Message-ID", &sanitize_header(mid));
    }
    out.push_str("MIME-Version: 1.0\r\n");
    if let Some(html) = row.body_html.as_deref().filter(|b| !b.is_empty()) {
        out.push_str("Content-Type: text/html; charset=utf-8\r\n\r\n");
        out.push_str(html);
    } else {
        out.push_str("Content-Type: text/plain; charset=utf-8\r\n\r\n");
        if let Some(text) = row.body_text.as_deref() {
            out.push_str(text);
        }
    }
    out.into_bytes()
}

/// CR/LF in a header value would inject extra headers — flatten them.
fn sanitize_header(value: &str) -> String {
    value.replace(['\r', '\n'], " ")
}

fn push_header(out: &mut String, name: &str, value: &str) {
    out.push_str(name);
    out.push_str(": ");
    out.push_str(value);
    out.push_str("\r\n");
}

/// RFC 2047 encoded-word for non-ASCII header text; ASCII passes through.
fn encode_header_value(value: &str) -> String {
    let clean = sanitize_header(value);
    if clean.is_ascii() {
        clean
    } else {
        format!("=?UTF-8?B?{}?=", B64.encode(clean.as_bytes()))
    }
}

/// Split a display address into (name, email): `Name <email>`, bare email,
/// or a bare name.
fn split_address(raw: &str) -> (Option<String>, Option<String>) {
    let raw = raw.trim();
    if let Some((name, rest)) = raw.rsplit_once('<') {
        let email = rest.trim_end_matches('>').trim();
        let name = name.trim().trim_matches('"');
        (
            (!name.is_empty()).then(|| name.to_string()),
            (!email.is_empty()).then(|| email.to_string()),
        )
    } else if raw.contains('@') {
        (None, Some(raw.to_string()))
    } else if raw.is_empty() {
        (None, None)
    } else {
        (Some(raw.to_string()), None)
    }
}

/// Address column JSON → (name, email) pairs. Shapes mirror
/// `push/diff.rs::sender_label`: an array of strings or {name,email}
/// objects, a single `{"raw": "…"}` object, or a bare string.
fn address_entries(json_text: Option<&str>) -> Vec<(Option<String>, Option<String>)> {
    let raw = json_text.unwrap_or("").trim();
    if raw.is_empty() {
        return Vec::new();
    }
    match serde_json::from_str::<Json>(raw) {
        Ok(Json::Array(entries)) => entries
            .iter()
            .map(|entry| match entry {
                Json::String(s) => split_address(s),
                other => {
                    let name = other.get("name").and_then(Json::as_str).unwrap_or("");
                    let email = other.get("email").and_then(Json::as_str).unwrap_or("");
                    (
                        (!name.is_empty()).then(|| name.to_string()),
                        (!email.is_empty()).then(|| email.to_string()),
                    )
                }
            })
            .filter(|(n, e)| n.is_some() || e.is_some())
            .collect(),
        Ok(Json::Object(obj)) => obj
            .get("raw")
            .and_then(Json::as_str)
            .map(|s| vec![split_address(s)])
            .unwrap_or_default(),
        Ok(Json::String(s)) => vec![split_address(&s)],
        _ => vec![split_address(raw)],
    }
}

/// One mailbox as an RFC 5322 address; only the display name is encoded.
fn format_mailbox(name: Option<&str>, email: Option<&str>) -> String {
    match (name, email) {
        (Some(n), Some(e)) => format!("{} <{e}>", encode_header_value(n)),
        (None, Some(e)) => sanitize_header(e),
        (Some(n), None) => encode_header_value(n),
        (None, None) => String::new(),
    }
}

fn header_address_list(json_text: Option<&str>) -> Option<String> {
    let list = address_entries(json_text)
        .iter()
        .map(|(n, e)| format_mailbox(n.as_deref(), e.as_deref()))
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(", ");
    (!list.is_empty()).then_some(list)
}

/// From header value: the sender address, or a placeholder when unknown.
fn header_from(from_json: Option<&str>) -> String {
    header_address_list(from_json).unwrap_or_else(|| "unknown@localhost".to_string())
}

/// mbox separator address: the sender's bare email, else a placeholder.
fn mbox_from_addr(from_json: Option<&str>) -> String {
    address_entries(from_json)
        .first()
        .and_then(|(_, email)| email.clone())
        .filter(|e| e.contains('@') && !e.contains(['\r', '\n']))
        .unwrap_or_else(|| "unknown@localhost".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{TEST_MASTER_KEY, install_test_master_key};
    use crate::kernel::App;
    use crate::kv::MemoryKv;
    use crate::storage::{DbPool, Storage};
    use std::sync::Arc;

    struct Fixture {
        state: AuthState,
        db: DbPool,
        user_id: String,
        account_id: String,
        dek: Vec<u8>,
        data_dir: tempfile::TempDir,
        staging: tempfile::TempDir,
    }

    fn sqlite_pool(db: &DbPool) -> &sqlx::SqlitePool {
        match db {
            DbPool::Sqlite(pool) => pool,
            #[cfg(feature = "mysql")]
            DbPool::Mysql(_) => panic!("expected sqlite"),
            #[cfg(feature = "postgres")]
            DbPool::Postgres(_) => panic!("expected sqlite in tests"),
        }
    }

    async fn seed() -> Fixture {
        install_test_master_key();
        let data_dir = tempfile::tempdir().unwrap();
        let staging = tempfile::tempdir().unwrap();
        let storage = Storage::new("sqlite::memory:").await.unwrap();
        storage.run_migrations().await.unwrap();
        let db = storage.pool().clone();
        let config = crate::config::Config {
            listen_addr: "127.0.0.1:0".into(),
            database_url: "sqlite::memory:".into(),
            data_dir: data_dir.path().to_string_lossy().into_owned(),
            min_password_length: 8,
            sync_max_concurrent: 3,
            sync_poll_secs: 300,
            max_attachment_bytes: 25 * 1024 * 1024,
            redis_url: None,
            master_key: TEST_MASTER_KEY.to_vec(),
            ms_oauth: None,
            yandex_oauth: None,
            captcha: crate::config::CaptchaConfig::None,
            vapid_subject: "mailto:test@example.com".to_string(),
        };
        let state = AuthState::new(
            db.clone(),
            &config,
            Arc::new(App::new()),
            Arc::new(MemoryKv::new()),
        )
        .unwrap();
        let pool = sqlite_pool(&db).clone();

        let user_id = store::new_uuid_text();
        sqlx::query(
            "INSERT INTO lyra_user (id, username, password_hash, ui_state) \
             VALUES (?, ?, 'hash', ?)",
        )
        .bind(&user_id)
        .bind(format!("backup-{user_id}"))
        .bind(r#"{"theme":"dark"}"#)
        .execute(&pool)
        .await
        .unwrap();
        let dek = crypto::generate_key();
        let kek = crypto::derive_user_kek(TEST_MASTER_KEY, &user_id);
        let wrapped = crypto::wrap_dek(&kek, &dek).unwrap();
        sqlx::query("UPDATE lyra_user SET encrypted_dek = ? WHERE id = ?")
            .bind(&wrapped)
            .bind(&user_id)
            .execute(&pool)
            .await
            .unwrap();

        let account_id = store::new_uuid_text();
        let credential = serde_json::to_string(
            &crypto::encrypt(&dek, br#"{"username":"u@example.com","password":"secret"}"#).unwrap(),
        )
        .unwrap();
        sqlx::query(
            "INSERT INTO mail_account (\
                 id, user_id, display_name, email_address, protocol, auth_type, \
                 credential, imap_host, imap_port, imap_security, \
                 smtp_host, smtp_port, smtp_security, \
                 is_active, sync_enabled, receive_protocol, send_protocol\
             ) VALUES (?, ?, 'Test Account', 'u@example.com', 'imap', 'password', \
                       ?, 'imap.example.com', 993, 'tls', \
                       'smtp.example.com', 465, 'tls', 1, 1, 'imap', 'smtp')",
        )
        .bind(&account_id)
        .bind(&user_id)
        .bind(&credential)
        .execute(&pool)
        .await
        .unwrap();

        store::upsert_folder(&db, &account_id, "INBOX", None, &[])
            .await
            .unwrap();

        Fixture {
            state,
            db,
            user_id,
            account_id,
            dek: dek.to_vec(),
            data_dir,
            staging,
        }
    }

    fn read_json(fx: &Fixture, rel: &str) -> Json {
        let text = std::fs::read_to_string(fx.staging.path().join(rel)).unwrap();
        serde_json::from_str(&text).unwrap()
    }

    /// Seed one contact (with photo blob), one calendar + event, and one
    /// message with an attachment blob.
    async fn seed_full_content(fx: &Fixture) {
        let pool = sqlite_pool(&fx.db).clone();

        let photo_rel = blobs::store(fx.data_dir.path(), &fx.account_id, b"png-bytes")
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO contact (id, account_id, external_id, vcard_blob, display_name, photo_path) \
             VALUES (?, ?, 'c1.vcf', ?, 'Ada', ?)",
        )
        .bind(store::new_uuid_text())
        .bind(&fx.account_id)
        .bind("BEGIN:VCARD\r\nVERSION:4.0\r\nFN:Ada\r\nEND:VCARD")
        .bind(&photo_rel)
        .execute(&pool)
        .await
        .unwrap();

        let calendar_id = store::new_uuid_text();
        sqlx::query(
            "INSERT INTO calendar (id, account_id, external_id, name, color, description, timezone) \
             VALUES (?, ?, 'cal1', 'Personal', '#ff0000', 'desc', 'UTC')",
        )
        .bind(&calendar_id)
        .bind(&fx.account_id)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO calendar_event (id, account_id, external_id, icalendar_blob, summary, calendar_id) \
             VALUES (?, ?, 'e1', 'BEGIN:VEVENT\r\nUID:e1\r\nEND:VEVENT', 'Evt', ?)",
        )
        .bind(store::new_uuid_text())
        .bind(&fx.account_id)
        .bind(&calendar_id)
        .execute(&pool)
        .await
        .unwrap();

        let folder_id = store::get_folder_id(&fx.db, &fx.account_id, "INBOX")
            .await
            .unwrap();
        let msg = crate::imap::ImapMessage {
            uid: 1,
            message_id: Some("<m1@example.com>".into()),
            subject: Some("Hello".into()),
            from: Some("a@example.com".into()),
            to: Some("u@example.com".into()),
            cc: None,
            date: None,
            in_reply_to: None,
            references: None,
            flags: vec![],
            size: None,
            body: None,
            body_text: None,
            body_html: None,
            has_attachments: true,
            attachments: vec![],
        };
        store::upsert_message(&fx.db, &fx.account_id, &folder_id, &msg)
            .await
            .unwrap();
        let message_id: String = sqlx::query_scalar("SELECT id FROM message WHERE account_id = ?")
            .bind(&fx.account_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        let att_rel = blobs::store(fx.data_dir.path(), &fx.account_id, b"attachment-bytes")
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO attachment (id, message_id, filename, storage_path) \
             VALUES (?, ?, 'a.bin', ?)",
        )
        .bind(store::new_uuid_text())
        .bind(&message_id)
        .bind(&att_rel)
        .execute(&pool)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn collect_writes_settings_accounts_contacts_calendars_blobs() {
        let fx = seed().await;
        seed_full_content(&fx).await;

        let counts = collect(&fx.state, &fx.user_id, fx.staging.path())
            .await
            .unwrap();

        assert_eq!(counts.accounts, 1);
        assert_eq!(counts.contacts, 1);
        assert_eq!(counts.calendars, 1);
        assert_eq!(counts.blobs, 2);
        assert!(counts.warnings.is_empty(), "{:?}", counts.warnings);

        // settings.json roundtrips ui_state.
        let settings = read_json(&fx, "settings.json");
        assert_eq!(settings["ui_state"], json!({"theme": "dark"}));

        // accounts/0.json carries the DECRYPTED inner credential value.
        let account: Json = read_json(&fx, "accounts/0.json");
        assert_eq!(account["index"], json!(0));
        assert_eq!(account["email_address"], json!("u@example.com"));
        assert_eq!(
            account["credential"],
            json!({"username": "u@example.com", "password": "secret"})
        );
        assert_eq!(account["smtp_credential"], Json::Null);
        assert_eq!(account["pim_credential"], Json::Null);
        assert_eq!(account["folders"][0]["external_id"], json!("INBOX"));
        assert_eq!(account["folders"][0]["role"], json!("inbox"));

        // contacts.vcf holds the vCard text.
        let vcf = std::fs::read_to_string(fx.staging.path().join("contacts.vcf")).unwrap();
        assert!(vcf.contains("BEGIN:VCARD"));
        assert!(vcf.contains("FN:Ada"));

        // calendars/0.ics wraps the VEVENT; calendars.json indexes it.
        let ics = std::fs::read_to_string(fx.staging.path().join("calendars/0.ics")).unwrap();
        assert!(ics.starts_with("BEGIN:VCALENDAR\r\nVERSION:2.0\r\n"));
        assert!(ics.contains("UID:e1"));
        assert!(ics.ends_with("END:VCALENDAR\r\n"));
        let calendars = read_json(&fx, "calendars.json");
        assert_eq!(calendars[0]["index"], json!(0));
        assert_eq!(calendars[0]["name"], json!("Personal"));
        assert_eq!(calendars[0]["timezone"], json!("UTC"));

        // blobs/<sha256> for both the photo and the attachment.
        let blobs_dir = fx.staging.path().join("blobs");
        assert!(blobs_dir.join(blobs::sha256_hex(b"png-bytes")).is_file());
        assert!(
            blobs_dir
                .join(blobs::sha256_hex(b"attachment-bytes"))
                .is_file()
        );
    }

    #[tokio::test]
    async fn missing_blob_is_a_warning_not_a_failure() {
        let fx = seed().await;
        let pool = sqlite_pool(&fx.db).clone();
        sqlx::query(
            "INSERT INTO contact (id, account_id, external_id, vcard_blob, photo_path) \
             VALUES (?, ?, 'c1.vcf', 'BEGIN:VCARD\r\nEND:VCARD', 'blobs/x/yy/missing')",
        )
        .bind(store::new_uuid_text())
        .bind(&fx.account_id)
        .execute(&pool)
        .await
        .unwrap();

        let counts = collect(&fx.state, &fx.user_id, fx.staging.path())
            .await
            .unwrap();
        assert_eq!(counts.blobs, 0);
        assert_eq!(counts.warnings.len(), 1);
        assert!(counts.warnings[0].contains("missing"));
    }

    fn imap_msg(uid: u32) -> crate::imap::ImapMessage {
        crate::imap::ImapMessage {
            uid,
            message_id: None,
            subject: Some(format!("m{uid}")),
            from: Some("alice@example.com".into()),
            to: Some("u@example.com".into()),
            cc: None,
            date: None,
            in_reply_to: None,
            references: None,
            flags: vec![],
            size: None,
            body: None,
            body_text: None,
            body_html: None,
            has_attachments: false,
            attachments: vec![],
        }
    }

    /// Three messages: one with a real raw blob, one with parsed bodies only
    /// (reconstructed — no server in tests), one fully empty (headers-only
    /// reconstruction).
    #[tokio::test]
    async fn mail_export_resolves_raw_and_reconstructs() {
        let fx = seed().await;
        let pool = sqlite_pool(&fx.db).clone();
        let kv: Arc<dyn KvStore> = Arc::new(MemoryKv::new());
        let folder_id = store::get_folder_id(&fx.db, &fx.account_id, "INBOX")
            .await
            .unwrap();

        for uid in 1..=3u32 {
            store::upsert_message(&fx.db, &fx.account_id, &folder_id, &imap_msg(uid))
                .await
                .unwrap();
        }
        // msg1: raw bytes already in the blob store.
        let raw1 =
            b"From: alice@example.com\r\nSubject: raw one\r\n\r\nraw body bytes\r\n".to_vec();
        let rel1 = blobs::store(fx.data_dir.path(), &fx.account_id, &raw1)
            .await
            .unwrap();
        sqlx::query(
            "UPDATE message SET raw_blob_path = ?, is_read = 1, \
             date = '2026-09-01 10:00:00', message_id_header = '<m1@example.com>' \
             WHERE external_id = ?",
        )
        .bind(&rel1)
        .bind(store::imap_message_external_id(&folder_id, 1))
        .execute(&pool)
        .await
        .unwrap();
        // msg2: parsed bodies only → reconstructed.
        sqlx::query(
            "UPDATE message SET body_html = '<p>你好</p>', subject = '你好 世界', \
             is_starred = 1, date = '2026-09-02 10:00:00', \
             from_address = '{\"raw\":\"Alice <alice@example.com>\"}', \
             to_addresses = '[{\"name\":\"鲍勃\",\"email\":\"bob@example.com\"}]' \
             WHERE external_id = ?",
        )
        .bind(store::imap_message_external_id(&folder_id, 2))
        .execute(&pool)
        .await
        .unwrap();
        // msg3: fully empty → headers-only reconstruction.
        sqlx::query(
            "UPDATE message SET subject = NULL, from_address = NULL, to_addresses = NULL, \
             date = NULL, message_id_header = NULL WHERE external_id = ?",
        )
        .bind(store::imap_message_external_id(&folder_id, 3))
        .execute(&pool)
        .await
        .unwrap();

        let total = collect_mail(
            &fx.state,
            &fx.user_id,
            "job-1",
            &kv,
            fx.staging.path(),
            false,
        )
        .await
        .unwrap();
        assert_eq!(total, 3);

        let mail_dir = fx.staging.path().join("mail/0");
        let mbox = std::fs::read(mail_dir.join(format!("{folder_id}.mbox"))).unwrap();
        let text = String::from_utf8_lossy(&mbox);
        assert!(text.contains("raw body bytes")); // raw verbatim
        assert!(text.contains("From alice@example.com ")); // mbox separator
        assert!(text.contains("<p>你好</p>")); // reconstructed html body
        assert!(text.contains("Subject: =?UTF-8?B?")); // non-ASCII encoded-word
        assert!(text.contains("From: unknown@localhost")); // empty sender placeholder

        let meta =
            std::fs::read_to_string(mail_dir.join(format!("{folder_id}.meta.jsonl"))).unwrap();
        let lines: Vec<MetaLine> = meta
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 3);
        let raw_line = lines
            .iter()
            .find(|l| l.message_id.as_deref() == Some("<m1@example.com>"))
            .unwrap();
        assert_eq!(raw_line.sha256, blobs::sha256_hex(&raw1));
        assert_eq!(raw_line.flags, vec!["seen"]);
        assert!(!raw_line.reconstructed);
        assert_eq!(raw_line.date.as_deref(), Some("2026-09-01T10:00:00+00:00"));
        assert_eq!(lines.iter().filter(|l| l.reconstructed).count(), 2);
        assert!(lines.iter().any(|l| l.flags == ["flagged"]));

        // folders.json maps the folder UUID to its wire path + role.
        let folders = read_json(&fx, "mail/0/folders.json");
        assert_eq!(folders[folder_id.as_str()]["path"], json!("INBOX"));
        assert_eq!(folders[folder_id.as_str()]["role"], json!("inbox"));

        // Progress recorded after the folder.
        let progress = kv.get("backup:progress:job-1").await.unwrap().unwrap();
        let progress: Json = serde_json::from_str(&progress).unwrap();
        assert_eq!(progress["phase"], json!("mail"));
        assert_eq!(progress["account"], json!(0));
        assert_eq!(progress["messages_done"], json!(3));
    }

    #[test]
    fn reconstruct_encodes_non_ascii_headers() {
        let row = MailRow {
            id: "r1".into(),
            external_id: None,
            message_id_header: Some("<r1@example.com>".into()),
            subject: Some("你好 世界".into()),
            from_address: Some(r#"{"raw":"QQ邮箱管理员 <10000@qq.com>"}"#.into()),
            to_addresses: Some(r#"[{"name":"鲍勃","email":"bob@example.com"}]"#.into()),
            cc_addresses: None,
            date: Some("2026-09-01 10:00:00".into()),
            is_read: false,
            is_starred: false,
            body_text: Some("plain body".into()),
            body_html: None,
        };
        let raw = reconstruct_message(&row);
        let text = String::from_utf8(raw).unwrap();
        // Display name encoded; addr-spec stays bare.
        assert!(text.contains("From: =?UTF-8?B?"));
        assert!(text.contains("<10000@qq.com>"));
        assert!(text.contains("To: =?UTF-8?B?"));
        assert!(text.contains("<bob@example.com>"));
        assert!(text.contains("Subject: =?UTF-8?B?"));
        assert!(text.contains("Content-Type: text/plain; charset=utf-8"));
        assert!(text.ends_with("plain body"));
    }

    #[test]
    fn reconstruct_prefers_html_and_handles_empty() {
        let mut row = MailRow {
            id: "r2".into(),
            external_id: None,
            message_id_header: None,
            subject: None,
            from_address: None,
            to_addresses: None,
            cc_addresses: None,
            date: None,
            is_read: false,
            is_starred: false,
            body_text: Some("text".into()),
            body_html: Some("<b>html</b>".into()),
        };
        let html = String::from_utf8(reconstruct_message(&row)).unwrap();
        assert!(html.contains("Content-Type: text/html; charset=utf-8"));
        assert!(html.ends_with("<b>html</b>"));
        assert!(html.starts_with("From: unknown@localhost\r\n"));

        row.body_html = None;
        row.body_text = None;
        let empty = String::from_utf8(reconstruct_message(&row)).unwrap();
        assert!(empty.ends_with("Content-Type: text/plain; charset=utf-8\r\n\r\n"));
    }

    #[test]
    fn address_parsing_mirrors_sender_label_shapes() {
        assert_eq!(
            mbox_from_addr(Some(r#"{"raw":"Name <n@example.com>"}"#)),
            "n@example.com"
        );
        assert_eq!(
            mbox_from_addr(Some(r#"[{"name":"A","email":"a@example.com"}]"#)),
            "a@example.com"
        );
        assert_eq!(mbox_from_addr(Some("bare@example.com")), "bare@example.com");
        assert_eq!(mbox_from_addr(None), "unknown@localhost");
        assert_eq!(mbox_from_addr(Some("no-at-sign")), "unknown@localhost");
        assert_eq!(
            header_address_list(Some(r#"{"raw":"QQ邮箱管理员 <10000@qq.com>"}"#)).unwrap(),
            format!("{} <10000@qq.com>", encode_header_value("QQ邮箱管理员"))
        );
    }
}
