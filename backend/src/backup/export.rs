//! Export collectors: snapshot every section into a staging dir.
//! Spec: docs/superpowers/specs/2026-09-08-lyra-backup-export-import-design.md §3-§5.
//!
//! Layout written under `staging/`:
//! `settings.json`, `accounts/<n>.json`, `contacts.vcf`, `calendars/<n>.ics` +
//! `calendars.json`, `blobs/<sha256>`. Mail (`mail/<n>/…`) is collected by
//! [`collect_mail`]. Decrypted credentials and raw message bytes are never
//! logged.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
use sea_orm::sea_query::{Expr, Order, Query as Sq};
use sea_orm::{ColumnTrait, ConnectionTrait, ExprTrait, QueryResult, Value};
use serde_json::{Value as Json, json};
use uuid::Uuid;

use crate::auth::AuthState;
use crate::blobs;
use crate::crypto::{self, EncryptedCredential};
use crate::db_row::{IdParam, id_param};
use crate::entities::{
    attachment, calendar, calendar_event, contact, folder, lyra_user, mail_account, message,
};
use crate::storage::DbPool;

use super::BackupError;

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{TEST_MASTER_KEY, install_test_master_key};
    use crate::kernel::App;
    use crate::kv::MemoryKv;
    use crate::sync::store;
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
}
