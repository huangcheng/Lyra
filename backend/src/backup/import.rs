//! Import: decrypt the uploaded archive, validate the manifest, then merge
//! every section additively. Per-item failures are collected into the report,
//! never fatal. Staging (extracted dir + uploaded file) is removed in all
//! outcomes. Decrypted credentials and raw message bytes are never logged.
//! Spec: docs/superpowers/specs/2026-09-08-lyra-backup-export-import-design.md §6.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

use sea_orm::sea_query::{Alias, Expr, Func, Query as Sq};
use sea_orm::{ColumnTrait, ConnectionTrait, ExprTrait, QueryResult, Value};
use serde_json::{Value as Json, json};
use uuid::Uuid;

use crate::auth::AuthState;
use crate::entities::{
    attachment, calendar, calendar_event, contact, folder, lyra_user, mail_account, message,
};
use crate::storage::DbPool;
use crate::sync::store;

use super::format::{FORMAT_VERSION, Manifest, MetaLine, unescape_mbox_line};
use super::{BackupError, crypto};

/// Full import: `staging/upload-<upload_id>.lyra` → age-decrypt → zip →
/// manifest validation → additive merge. Progress and the final report live
/// in kv (`backup:progress:<job_id>` / `backup:report:<job_id>`), mirroring
/// [`super::export::run`]'s report discipline: ANY error writes a scrubbed
/// `{"ok":false,"error":…}` report before the error propagates.
pub async fn run(
    state: &AuthState,
    user_id: &str,
    job_id: &str,
    upload_id: &str,
    password: &str,
) -> Result<(), BackupError> {
    let kv = state.kv();
    let _ = kv
        .set(
            &format!("backup:progress:{job_id}"),
            &json!({"phase": "decrypting"}).to_string(),
            Some(3600),
        )
        .await;

    match run_inner(state, user_id, job_id, upload_id, password).await {
        Ok(report) => {
            let out = json!({"ok": true, "report": report});
            kv.set(&format!("backup:report:{job_id}"), &out.to_string(), None)
                .await
                .map_err(|e| BackupError::Internal(e.to_string()))?;
            Ok(())
        }
        Err(err) => {
            let report =
                json!({"ok": false, "error": crate::jobs::scrub_error_detail(&err.to_string())});
            let _ = kv
                .set(
                    &format!("backup:report:{job_id}"),
                    &report.to_string(),
                    None,
                )
                .await;
            Err(err)
        }
    }
}

/// Locate the staged upload, decrypt + extract into a fresh
/// `staging/import-<job_id>/` dir, merge, then remove BOTH the extraction
/// dir and the uploaded file in all outcomes.
async fn run_inner(
    state: &AuthState,
    user_id: &str,
    job_id: &str,
    upload_id: &str,
    password: &str,
) -> Result<Json, BackupError> {
    let staging = state.data_dir.join("backups").join("staging");
    // Both ids are path segments: they must be UUIDs so the archive and the
    // extraction dir can never traverse out of the staging dir (mirrors
    // `upload::upload_path` / `artifacts::artifact_path`).
    let upload_uuid = Uuid::parse_str(upload_id)
        .map_err(|_| BackupError::Internal("invalid upload id".into()))?;
    let job_uuid =
        Uuid::parse_str(job_id).map_err(|_| BackupError::Internal("invalid job id".into()))?;
    let upload = staging.join(format!("upload-{upload_uuid}.lyra"));
    if !upload.is_file() {
        return Err(BackupError::UploadIncomplete);
    }

    let dir = staging.join(format!("import-{job_uuid}"));
    tokio::fs::create_dir_all(&dir).await?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).await?;
    }

    let outcome = async {
        // age + zip are blocking (age reads the whole archive into memory).
        let dir_owned = dir.clone();
        let upload_owned = upload.clone();
        let password_owned = zeroize::Zeroizing::new(password.to_string());
        let manifest = tokio::task::spawn_blocking(move || {
            decrypt_and_open(&upload_owned, &dir_owned, password_owned.as_str())
        })
        .await
        .map_err(|e| BackupError::Internal(format!("decrypt/open task failed: {e}")))??;

        let _ = &manifest; // section counts are informational; merge is file-driven
        let mut report = ImportReport::default();
        merge_all(state, user_id, job_id, &dir, &mut report).await;
        Ok::<Json, BackupError>(serde_json::to_value(&report)?)
    }
    .await;

    let _ = tokio::fs::remove_dir_all(&dir).await;
    let _ = tokio::fs::remove_file(&upload).await;
    outcome
}

/// age-decrypt `upload` to `<dir>/archive.zip`, then validate + extract the
/// zip and return its manifest. The staging zip is removed once extracted.
/// Call from a blocking context.
fn decrypt_and_open(upload: &Path, dir: &Path, password: &str) -> Result<Manifest, BackupError> {
    let zip_path = dir.join("archive.zip");
    crypto::decrypt_file(upload, &zip_path, password)?;
    let result = extract_archive(&zip_path, dir, MAX_EXTRACTED_BYTES);
    let _ = std::fs::remove_file(&zip_path);
    result
}

/// Total extracted-size cap: a hostile zip can inflate far past its
/// compressed size, so bail out past 16 GiB of extracted payload.
const MAX_EXTRACTED_BYTES: u64 = 16 << 30;

/// Validate the manifest and extract every entry under `dir`, rejecting
/// zip-slip names (absolute paths, `..` components, backslashes) and
/// payloads exceeding `max_bytes` in total (zip-bomb guard).
fn extract_archive(zip_path: &Path, dir: &Path, max_bytes: u64) -> Result<Manifest, BackupError> {
    use std::io::Read as _;
    let file = std::fs::File::open(zip_path)?;
    let mut zip = zip::ZipArchive::new(file).map_err(|_| BackupError::CorruptArchive)?;
    let manifest: Manifest = {
        let mut entry = zip
            .by_name("manifest.json")
            .map_err(|_| BackupError::CorruptArchive)?;
        let mut text = String::new();
        entry
            .read_to_string(&mut text)
            .map_err(|_| BackupError::CorruptArchive)?;
        serde_json::from_str(&text).map_err(|_| BackupError::CorruptArchive)?
    };
    if manifest.app != "lyra" || manifest.format != FORMAT_VERSION {
        return Err(BackupError::UnsupportedFormat);
    }
    let mut total = 0u64;
    for i in 0..zip.len() {
        let entry = zip.by_index(i).map_err(|_| BackupError::CorruptArchive)?;
        let Some(rel) = sanitize_entry_name(entry.name()) else {
            return Err(BackupError::CorruptArchive);
        };
        let target = dir.join(&rel);
        if entry.name().ends_with('/') {
            std::fs::create_dir_all(&target)?;
            continue;
        }
        if let Some(parent) = rel.parent() {
            std::fs::create_dir_all(dir.join(parent))?;
        }
        let mut out = std::fs::File::create(&target)?;
        // Read at most one byte past the remaining budget, so the copy
        // stops early instead of writing an unbounded entry.
        let remaining = max_bytes.saturating_sub(total);
        let written = std::io::copy(&mut entry.take(remaining.saturating_add(1)), &mut out)?;
        total += written;
        if total > max_bytes {
            return Err(BackupError::CorruptArchive);
        }
    }
    Ok(manifest)
}

/// An entry name safe to extract under the staging dir: relative, no `..`,
/// no backslashes (which are path separators on Windows).
fn sanitize_entry_name(name: &str) -> Option<PathBuf> {
    if name.is_empty() || name.contains('\\') {
        return None;
    }
    let mut out = PathBuf::new();
    for comp in Path::new(name).components() {
        match comp {
            Component::Normal(seg) => out.push(seg),
            // CurDir / RootDir / Prefix / ParentDir are all rejected: the
            // writer only ever emits plain relative names.
            _ => return None,
        }
    }
    (!out.as_os_str().is_empty()).then_some(out)
}

// ── Merge machinery ──────────────────────────────────────────────────

/// Per-section additive-merge counters.
#[derive(Debug, Default, Clone, Copy, serde::Serialize)]
struct SectionCounts {
    inserted: u64,
    skipped: u64,
    /// Existing rows whose missing raw blob / attachments were backfilled
    /// from the archive (messages section; always 0 elsewhere).
    repaired: u64,
    failed: u64,
}

/// The final kv `backup:report:<job_id>` payload's `report` field.
#[derive(Debug, Default, serde::Serialize)]
struct ImportReport {
    settings: bool,
    accounts: SectionCounts,
    folders: SectionCounts,
    messages: SectionCounts,
    contacts: SectionCounts,
    calendars: SectionCounts,
    errors: Vec<String>,
}

/// Reported per-item errors are capped; the archive keeps coming.
const REPORT_ERROR_CAP: usize = 10;

impl ImportReport {
    /// Scrubbed at the boundary: error strings land in the kv report, so
    /// credential-like spans are redacted before they are ever stored.
    fn error(&mut self, msg: impl Into<String>) {
        if self.errors.len() < REPORT_ERROR_CAP {
            self.errors
                .push(crate::jobs::scrub_error_detail(&msg.into()));
        }
    }
}

/// `{"phase":"merging","section":…}` progress; best-effort.
async fn progress(state: &AuthState, job_id: &str, section: &str) {
    let _ = state
        .kv()
        .set(
            &format!("backup:progress:{job_id}"),
            &json!({"phase": "merging", "section": section}).to_string(),
            Some(3600),
        )
        .await;
}

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

/// Map a sync-store helper error into BackupError.
fn sync_err(err: crate::sync::SyncError) -> BackupError {
    match err {
        crate::sync::SyncError::Database(e) => BackupError::Db(e),
        other => BackupError::Internal(other.to_string()),
    }
}

/// UUID-column id bind: TEXT on SQLite/MySQL, native `Uuid` on Postgres.
fn id_bind(db: &DbPool, id: &str) -> Result<Value, BackupError> {
    store::id_value(db, id).map_err(sync_err)
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

fn row_opt_str(row: &QueryResult, col: &str) -> Result<Option<String>, BackupError> {
    row.try_get::<Option<String>>("", col).map_err(orm_err)
}

/// Merge every archive section additively; never fails — per-item failures
/// accumulate in the report.
async fn merge_all(
    state: &AuthState,
    user_id: &str,
    job_id: &str,
    dir: &Path,
    report: &mut ImportReport,
) {
    merge_settings(state, user_id, job_id, dir, report).await;
    let docs = load_account_docs(dir, report).await;
    let id_map = merge_accounts(state, user_id, job_id, &docs, report).await;
    let folder_maps = merge_folders(state, user_id, job_id, &docs, &id_map, dir, report).await;
    merge_messages(state, job_id, &id_map, &folder_maps, dir, report).await;
    merge_contacts(state, job_id, &id_map, dir, report).await;
    merge_calendars(state, job_id, &id_map, dir, report).await;
}

// ── Settings ─────────────────────────────────────────────────────────

/// `settings.json` → `lyra_user.ui_state` (the same column the auth
/// preferences PATCH writes), applied wholesale.
async fn merge_settings(
    state: &AuthState,
    user_id: &str,
    job_id: &str,
    dir: &Path,
    report: &mut ImportReport,
) {
    progress(state, job_id, "settings").await;
    let Ok(text) = tokio::fs::read_to_string(dir.join("settings.json")).await else {
        return; // section absent from the archive
    };
    let doc: Json = match serde_json::from_str(&text) {
        Ok(doc) => doc,
        Err(e) => {
            report.error(format!("settings.json: invalid json: {e}"));
            return;
        }
    };
    let ui_state = doc.get("ui_state").cloned().unwrap_or(Json::Null);
    if !ui_state.is_object() {
        report.error("settings.json: ui_state is not an object".to_string());
        return;
    }
    let user_bind = match id_bind(&state.db, user_id) {
        Ok(v) => v,
        Err(e) => {
            report.error(format!("settings: {e}"));
            return;
        }
    };
    let mut upd = Sq::update();
    upd.table(lyra_user::Entity)
        .value(lyra_user::Column::UiState, Expr::val(ui_state.to_string()))
        .value(lyra_user::Column::UpdatedAt, Expr::current_timestamp())
        .and_where(lyra_user::Column::Id.eq(user_bind));
    match state.db.orm().execute(&upd).await {
        Ok(_) => report.settings = true,
        Err(e) => report.error(format!("settings: {e}")),
    }
}

// ── Accounts ─────────────────────────────────────────────────────────

/// Parse `accounts/<n>.json` for every contiguous index, keeping index
/// alignment (parse failures become `None` entries + a failed count).
async fn load_account_docs(dir: &Path, report: &mut ImportReport) -> Vec<Option<Json>> {
    let mut docs = Vec::new();
    for n in 0usize.. {
        let path = dir.join("accounts").join(format!("{n}.json"));
        let Ok(text) = tokio::fs::read_to_string(&path).await else {
            break;
        };
        match serde_json::from_str(&text) {
            Ok(doc) => docs.push(Some(doc)),
            Err(e) => {
                report.error(format!("accounts/{n}.json: invalid json: {e}"));
                report.accounts.failed += 1;
                docs.push(None);
            }
        }
    }
    docs
}

/// One existing account as needed for the `(protocol, email)` merge key.
struct ExistingAccount {
    id: String,
    protocol: String,
    email_lower: String,
}

async fn load_existing_accounts(
    db: &DbPool,
    user_id: &str,
) -> Result<Vec<ExistingAccount>, BackupError> {
    let mut sel = Sq::select();
    sel.columns([
        mail_account::Column::Id,
        mail_account::Column::Protocol,
        mail_account::Column::EmailAddress,
    ])
    .from(mail_account::Entity)
    .and_where(mail_account::Column::UserId.eq(id_bind(db, user_id)?));
    let rows = db.orm().query_all(&sel).await.map_err(orm_err)?;
    rows.iter()
        .map(|row| {
            Ok(ExistingAccount {
                id: row_id(row, "id")?,
                protocol: row.try_get::<String>("", "protocol").map_err(orm_err)?,
                email_lower: row
                    .try_get::<String>("", "email_address")
                    .map_err(orm_err)?
                    .to_lowercase(),
            })
        })
        .collect()
}

/// Merge `accounts/<n>.json`: match on `(protocol, email_address)`
/// case-insensitive; matched accounts keep their id and credentials,
/// unmatched are inserted with credentials re-encrypted under THIS
/// instance's user DEK. Returns `id_map[archive_index] = local account id`.
async fn merge_accounts(
    state: &AuthState,
    user_id: &str,
    job_id: &str,
    docs: &[Option<Json>],
    report: &mut ImportReport,
) -> HashMap<usize, String> {
    progress(state, job_id, "accounts").await;
    let db = &state.db;
    let mut id_map = HashMap::new();
    if docs.iter().all(Option::is_none) {
        return id_map;
    }
    let existing = match load_existing_accounts(db, user_id).await {
        Ok(rows) => rows,
        Err(e) => {
            report.error(format!("accounts: {e}"));
            report.accounts.failed += docs.iter().flatten().count() as u64;
            return id_map;
        }
    };
    let dek = match AuthState::get_user_dek(db, user_id).await {
        Ok(dek) => dek,
        Err(e) => {
            report.error(format!("accounts: cannot load user DEK: {e}"));
            report.accounts.failed += docs.iter().flatten().count() as u64;
            return id_map;
        }
    };
    let mut existing = existing;
    for (n, doc) in docs.iter().enumerate() {
        let Some(doc) = doc else { continue };
        let Some(email) = doc.get("email_address").and_then(Json::as_str) else {
            report.error(format!("accounts/{n}.json: missing email_address"));
            report.accounts.failed += 1;
            continue;
        };
        let protocol = doc.get("protocol").and_then(Json::as_str).unwrap_or("imap");
        if let Some(found) = existing
            .iter()
            .find(|a| a.protocol == protocol && a.email_lower == email.to_lowercase())
        {
            id_map.insert(n, found.id.clone());
            report.accounts.skipped += 1;
            continue;
        }
        match insert_account(db, user_id, doc, protocol, &dek).await {
            Ok(id) => {
                existing.push(ExistingAccount {
                    id: id.clone(),
                    protocol: protocol.to_string(),
                    email_lower: email.to_lowercase(),
                });
                id_map.insert(n, id);
                report.accounts.inserted += 1;
            }
            Err(e) => {
                report.error(format!("accounts/{n}.json: {e}"));
                report.accounts.failed += 1;
            }
        }
    }
    id_map
}

/// Re-encrypt one archive credential value (export's decrypted inner value)
/// under this instance's DEK. Null → NULL column. A JSON string is the
/// mirrored plain-string case and is re-encrypted WITHOUT its JSON quotes.
fn encrypt_credential(dek: &[u8], value: &Json) -> Result<Option<String>, BackupError> {
    if value.is_null() {
        return Ok(None);
    }
    let plaintext = match value {
        Json::String(s) => s.as_bytes().to_vec(),
        other => serde_json::to_vec(other)?,
    };
    let envelope =
        crate::crypto::encrypt(dek, &plaintext).map_err(|e| BackupError::Crypto(e.to_string()))?;
    Ok(Some(serde_json::to_string(&envelope)?))
}

/// Insert one `mail_account` row from an archive doc (timestamps use the
/// table defaults, `last_sync_at` stays NULL, `is_active = true`).
async fn insert_account(
    db: &DbPool,
    user_id: &str,
    doc: &Json,
    protocol: &str,
    dek: &[u8],
) -> Result<String, BackupError> {
    let s = |key: &str| -> Value {
        Value::String(doc.get(key).and_then(Json::as_str).map(str::to_owned))
    };
    let port = |key: &str| -> Value {
        Value::Int(
            doc.get(key)
                .and_then(Json::as_i64)
                .and_then(|v| i32::try_from(v).ok()),
        )
    };
    let credential = encrypt_credential(dek, doc.get("credential").unwrap_or(&Json::Null))?
        .ok_or_else(|| BackupError::Internal("account has no credential".into()))?;
    let smtp_credential =
        encrypt_credential(dek, doc.get("smtp_credential").unwrap_or(&Json::Null))?;
    let pim_credential = encrypt_credential(dek, doc.get("pim_credential").unwrap_or(&Json::Null))?;
    let sync_enabled = doc
        .get("sync_enabled")
        .and_then(Json::as_bool)
        .unwrap_or(true);
    let auth_type = doc
        .get("auth_type")
        .and_then(Json::as_str)
        .unwrap_or("password")
        .to_string();
    let receive_protocol = doc
        .get("receive_protocol")
        .and_then(Json::as_str)
        .unwrap_or(protocol)
        .to_string();
    let send_protocol = doc
        .get("send_protocol")
        .and_then(Json::as_str)
        .unwrap_or("smtp")
        .to_string();

    let id = store::new_uuid_text();
    let mut ins = Sq::insert();
    ins.into_table(mail_account::Entity)
        .columns([
            mail_account::Column::Id,
            mail_account::Column::UserId,
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
            mail_account::Column::IsActive,
            mail_account::Column::SyncEnabled,
            mail_account::Column::ReceiveProtocol,
            mail_account::Column::SendProtocol,
        ])
        .values_panic([
            Expr::val(id_bind(db, &id)?),
            Expr::val(id_bind(db, user_id)?),
            Expr::val(s("display_name")),
            Expr::val(s("email_address")),
            Expr::val(protocol),
            Expr::val(auth_type),
            Expr::val(credential),
            Expr::val(s("imap_host")),
            Expr::val(port("imap_port")),
            Expr::val(s("imap_security")),
            Expr::val(s("jmap_base_url")),
            Expr::val(s("smtp_host")),
            Expr::val(port("smtp_port")),
            Expr::val(s("smtp_security")),
            Expr::val(s("smtp_auth_type")),
            Expr::val(smtp_credential),
            Expr::val(pim_credential),
            Expr::val(s("signature")),
            Expr::val(s("carddav_url")),
            Expr::val(s("caldav_url")),
            Expr::val(true),
            Expr::val(sync_enabled),
            Expr::val(receive_protocol),
            Expr::val(send_protocol),
        ]);
    db.orm().execute(&ins).await.map_err(orm_err)?;
    Ok(id)
}

// ── Folders ──────────────────────────────────────────────────────────

/// One existing folder as needed for the `(account, external_id)` merge key
/// plus the name+parent fallback.
struct ExistingFolder {
    id: String,
    external_id: Option<String>,
    name: String,
    parent_id: Option<String>,
}

async fn load_existing_folders(
    db: &DbPool,
    account_id: &str,
) -> Result<Vec<ExistingFolder>, BackupError> {
    let mut sel = Sq::select();
    sel.columns([
        folder::Column::Id,
        folder::Column::ExternalId,
        folder::Column::Name,
        folder::Column::ParentId,
    ])
    .from(folder::Entity)
    .and_where(folder::Column::AccountId.eq(id_bind(db, account_id)?));
    let rows = db.orm().query_all(&sel).await.map_err(orm_err)?;
    rows.iter()
        .map(|row| {
            Ok(ExistingFolder {
                id: row_id(row, "id")?,
                external_id: row_opt_str(row, "external_id")?,
                name: row.try_get::<String>("", "name").map_err(orm_err)?,
                parent_id: row_opt_id(row, "parent_id")?,
            })
        })
        .collect()
}

/// Per archive account: match folders by `(account_id, external_id)`
/// (fallback: name + resolved parent) and insert the missing ones.
/// Returns `folder_map[archive_index][archive_folder_uuid] = local_uuid`.
async fn merge_folders(
    state: &AuthState,
    user_id: &str,
    job_id: &str,
    docs: &[Option<Json>],
    id_map: &HashMap<usize, String>,
    dir: &Path,
    report: &mut ImportReport,
) -> HashMap<usize, HashMap<String, String>> {
    let _ = user_id;
    progress(state, job_id, "folders").await;
    let db = &state.db;
    let mut folder_maps = HashMap::new();
    for (n, account_id) in id_map {
        let Some(Some(doc)) = docs.get(*n) else {
            continue;
        };
        let Some(entries) = doc.get("folders").and_then(Json::as_array) else {
            continue;
        };
        // mail/<n>/folders.json: archive folder uuid → {path, role}; the
        // role is the export-time EFFECTIVE role, used only as a fallback.
        let folders_json: Json =
            tokio::fs::read_to_string(dir.join("mail").join(n.to_string()).join("folders.json"))
                .await
                .ok()
                .and_then(|text| serde_json::from_str(&text).ok())
                .unwrap_or(Json::Null);

        let mut local = match load_existing_folders(db, account_id).await {
            Ok(rows) => rows,
            Err(e) => {
                report.error(format!("folders: account {n}: {e}"));
                report.folders.failed += entries.len() as u64;
                continue;
            }
        };
        let mut map: HashMap<String, String> = HashMap::new();
        // Archive external_id → archive folder uuid, for parent resolution.
        let archive_id_by_external: HashMap<&str, &str> = entries
            .iter()
            .filter_map(|f| {
                let ext = f.get("external_id").and_then(Json::as_str)?;
                let id = f.get("id").and_then(Json::as_str)?;
                Some((ext, id))
            })
            .collect();

        let mut unresolved: Vec<&Json> = entries.iter().collect();
        loop {
            let before = unresolved.len();
            let mut still = Vec::new();
            for f in unresolved {
                match merge_one_folder(
                    db,
                    account_id,
                    f,
                    &folders_json,
                    &archive_id_by_external,
                    &mut local,
                    &mut map,
                )
                .await
                {
                    Ok(true) => report.folders.inserted += 1,
                    Ok(false) => report.folders.skipped += 1,
                    Err(FolderMerge::Retry) => still.push(f),
                    Err(FolderMerge::Failed(e)) => {
                        report.error(format!("folders: account {n}: {e}"));
                        report.folders.failed += 1;
                    }
                }
            }
            if still.len() == before {
                // Fixpoint: parents unresolvable — create at root.
                for f in still {
                    let name = f
                        .get("name")
                        .and_then(Json::as_str)
                        .unwrap_or("(unnamed)")
                        .to_string();
                    match merge_one_folder_root(db, account_id, f, &mut local, &mut map).await {
                        Ok(true) => {
                            report.folders.inserted += 1;
                            report.error(format!(
                                "folders: account {n}: folder '{name}' parent unresolved, created at root"
                            ));
                        }
                        Ok(false) => report.folders.skipped += 1,
                        Err(e) => {
                            report.error(format!("folders: account {n}: {e}"));
                            report.folders.failed += 1;
                        }
                    }
                }
                break;
            }
            unresolved = still;
        }
        folder_maps.insert(*n, map);
    }
    folder_maps
}

enum FolderMerge {
    Retry,
    Failed(String),
}

/// Parent resolution for one archive folder.
enum ParentResolution {
    /// No parent in the archive: create/match at root.
    Root,
    /// Resolved to a local folder id.
    Resolved(String),
    /// Parent not mapped yet — retry on the next fixpoint pass.
    Retry,
}

/// Resolve the archive parent external id to a local folder id: already
/// mapped in this run, or a pre-existing local folder.
fn resolve_parent(
    parent_external_id: Option<&str>,
    archive_id_by_external: &HashMap<&str, &str>,
    local: &[ExistingFolder],
    map: &HashMap<String, String>,
) -> ParentResolution {
    let Some(parent_ext) = parent_external_id else {
        return ParentResolution::Root;
    };
    if let Some(local_id) = archive_id_by_external
        .get(parent_ext)
        .and_then(|archive_id| map.get(*archive_id))
    {
        return ParentResolution::Resolved(local_id.clone());
    }
    if let Some(ex) = local
        .iter()
        .find(|l| l.external_id.as_deref() == Some(parent_ext))
    {
        return ParentResolution::Resolved(ex.id.clone());
    }
    ParentResolution::Retry
}

/// Match or insert one archive folder. `Ok(true)` inserted, `Ok(false)`
/// matched existing, `Err(Retry)` when the parent is not resolvable yet.
async fn merge_one_folder(
    db: &DbPool,
    account_id: &str,
    f: &Json,
    folders_json: &Json,
    archive_id_by_external: &HashMap<&str, &str>,
    local: &mut Vec<ExistingFolder>,
    map: &mut HashMap<String, String>,
) -> Result<bool, FolderMerge> {
    let Some(archive_id) = f.get("id").and_then(Json::as_str) else {
        return Err(FolderMerge::Failed("folder entry has no id".into()));
    };
    let parent = match resolve_parent(
        f.get("parent_external_id").and_then(Json::as_str),
        archive_id_by_external,
        local,
        map,
    ) {
        ParentResolution::Root => None,
        ParentResolution::Resolved(id) => Some(id),
        ParentResolution::Retry => return Err(FolderMerge::Retry),
    };
    let external_id = f.get("external_id").and_then(Json::as_str);
    let name = f.get("name").and_then(Json::as_str).unwrap_or("(unnamed)");
    let existing = local.iter().find(|l| match external_id {
        Some(ext) if !ext.is_empty() => l.external_id.as_deref() == Some(ext),
        _ => l.name == name && l.parent_id == parent,
    });
    if let Some(ex) = existing {
        map.insert(archive_id.to_string(), ex.id.clone());
        return Ok(false);
    }
    let role = f.get("role").and_then(Json::as_str).map(str::to_owned);
    let role_override = f
        .get("role_override")
        .and_then(Json::as_str)
        .map(str::to_owned);
    let role = role.or_else(|| {
        folders_json
            .get(archive_id)
            .and_then(|v| v.get("role"))
            .and_then(Json::as_str)
            .map(str::to_owned)
    });
    let sort_order = f
        .get("sort_order")
        .and_then(Json::as_i64)
        .and_then(|v| i32::try_from(v).ok())
        .unwrap_or(0);
    insert_folder(
        db,
        account_id,
        external_id,
        name,
        parent.as_deref(),
        role,
        role_override,
        sort_order,
        local,
        map,
        archive_id,
    )
    .await
    .map_err(FolderMerge::Failed)?;
    Ok(true)
}

/// Fixpoint fallback: insert at root when the parent never resolved.
async fn merge_one_folder_root(
    db: &DbPool,
    account_id: &str,
    f: &Json,
    local: &mut Vec<ExistingFolder>,
    map: &mut HashMap<String, String>,
) -> Result<bool, String> {
    let Some(archive_id) = f.get("id").and_then(Json::as_str) else {
        return Err("folder entry has no id".into());
    };
    let external_id = f.get("external_id").and_then(Json::as_str);
    let name = f.get("name").and_then(Json::as_str).unwrap_or("(unnamed)");
    if let Some(ex) = local.iter().find(|l| match external_id {
        Some(ext) if !ext.is_empty() => l.external_id.as_deref() == Some(ext),
        _ => l.name == name && l.parent_id.is_none(),
    }) {
        map.insert(archive_id.to_string(), ex.id.clone());
        return Ok(false);
    }
    let role = f.get("role").and_then(Json::as_str).map(str::to_owned);
    let role_override = f
        .get("role_override")
        .and_then(Json::as_str)
        .map(str::to_owned);
    let sort_order = f
        .get("sort_order")
        .and_then(Json::as_i64)
        .and_then(|v| i32::try_from(v).ok())
        .unwrap_or(0);
    insert_folder(
        db,
        account_id,
        external_id,
        name,
        None,
        role,
        role_override,
        sort_order,
        local,
        map,
        archive_id,
    )
    .await?;
    Ok(true)
}

#[allow(clippy::too_many_arguments)]
async fn insert_folder(
    db: &DbPool,
    account_id: &str,
    external_id: Option<&str>,
    name: &str,
    parent_id: Option<&str>,
    role: Option<String>,
    role_override: Option<String>,
    sort_order: i32,
    local: &mut Vec<ExistingFolder>,
    map: &mut HashMap<String, String>,
    archive_id: &str,
) -> Result<(), String> {
    let id = store::new_uuid_text();
    let parent_bind = store::opt_id_value(db, parent_id).map_err(|e| e.to_string())?;
    let mut ins = Sq::insert();
    ins.into_table(folder::Entity)
        .columns([
            folder::Column::Id,
            folder::Column::AccountId,
            folder::Column::ExternalId,
            folder::Column::Name,
            folder::Column::ParentId,
            folder::Column::Role,
            folder::Column::RoleOverride,
            folder::Column::SortOrder,
        ])
        .values_panic([
            Expr::val(id_bind(db, &id).map_err(|e| e.to_string())?),
            Expr::val(id_bind(db, account_id).map_err(|e| e.to_string())?),
            Expr::val(external_id),
            Expr::val(name),
            Expr::val(parent_bind),
            Expr::val(role),
            Expr::val(role_override),
            Expr::val(sort_order),
        ]);
    db.orm()
        .execute(&ins)
        .await
        .map_err(|e| orm_err(e).to_string())?;
    local.push(ExistingFolder {
        id: id.clone(),
        external_id: external_id.map(str::to_owned),
        name: name.to_string(),
        parent_id: parent_id.map(str::to_owned),
    });
    map.insert(archive_id.to_string(), id);
    Ok(())
}

// ── Messages ─────────────────────────────────────────────────────────

/// Split an mbox into message chunks: the (still escaped, still
/// CRLF-terminated) bytes between `From <addr> <epoch>` separator lines.
fn split_mbox(mbox: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut body_start: Option<usize> = None;
    let mut pos = 0usize;
    while pos < mbox.len() {
        let newline = mbox[pos..]
            .iter()
            .position(|b| *b == b'\n')
            .map(|p| pos + p);
        let (line, next) = match newline {
            Some(end) => (&mbox[pos..end], end + 1),
            None => (&mbox[pos..], mbox.len()),
        };
        let bare = line.strip_suffix(b"\r").unwrap_or(line);
        // mboxrd: a separator is a line starting with "From " — escaped body
        // lines always carry a '>' prefix, so they never match.
        if bare.starts_with(b"From ") {
            if let Some(start) = body_start.take() {
                out.push(&mbox[start..pos]);
            }
            body_start = Some(next);
        }
        pos = next;
    }
    if let Some(start) = body_start {
        out.push(&mbox[start..]);
    }
    out
}

/// Recover the raw RFC822 bytes of one mbox chunk: strip the writer's single
/// trailing CRLF, then unescape exactly one leading `>` per line.
fn recover_raw(chunk: &[u8]) -> Vec<u8> {
    let chunk = chunk.strip_suffix(b"\r\n").unwrap_or(chunk);
    let mut out = Vec::with_capacity(chunk.len());
    for (i, line) in chunk.split(|b| *b == b'\n').enumerate() {
        if i > 0 {
            out.push(b'\n');
        }
        out.extend_from_slice(unescape_mbox_line(line));
    }
    out
}

/// The row an incoming archive message matched, with what it still lacks.
struct ExistingMessage {
    id: String,
    has_raw: bool,
    has_attachments: bool,
}

/// The account's existing copy of this message, if any: same
/// `import:<sha256>` external id (re-import) or same Message-ID header
/// (already-synced copy).
async fn find_existing_message(
    db: &DbPool,
    account_id: &str,
    external_id: &str,
    message_id_header: Option<&str>,
) -> Result<Option<ExistingMessage>, BackupError> {
    let mut cond = Expr::col(message::Column::ExternalId).eq(external_id);
    if let Some(mid) = message_id_header.filter(|m| !m.is_empty()) {
        cond = cond.or(Expr::col(message::Column::MessageIdHeader).eq(mid));
    }
    let mut sel = Sq::select();
    sel.column(message::Column::Id)
        .column(message::Column::RawBlobPath)
        .from(message::Entity)
        .and_where(message::Column::AccountId.eq(id_bind(db, account_id)?))
        .and_where(cond);
    let Some(row) = db.orm().query_one(&sel).await.map_err(orm_err)? else {
        return Ok(None);
    };
    let id = row_id(&row, "id")?;
    let has_raw = row_opt_str(&row, "raw_blob_path")?.is_some_and(|p| !p.is_empty());
    let mut cnt = Sq::select();
    cnt.expr_as(
        Func::count(Expr::col(attachment::Column::Id)),
        Alias::new("n"),
    )
    .from(attachment::Entity)
    .and_where(attachment::Column::MessageId.eq(id_bind(db, &id)?));
    let row = db.orm().query_one(&cnt).await.map_err(orm_err)?;
    let n = row
        .and_then(|r| r.try_get::<i64>("", "n").ok())
        .unwrap_or(0);
    Ok(Some(ExistingMessage {
        id,
        has_raw,
        has_attachments: n > 0,
    }))
}

/// Outcome of one message: inserted/skipped/repaired plus non-fatal blob
/// warnings.
struct MessageOutcome {
    inserted: bool,
    repaired: bool,
    warnings: Vec<String>,
}

/// Partition parsed attachments by whether the archive `blobs/` dir carries
/// their content hash; missing ones become warnings, never fatal.
fn partition_attachments(
    attachments: Vec<crate::imap::ExtractedAttachment>,
    blobs_dir: &Path,
) -> (Vec<crate::imap::ExtractedAttachment>, Vec<String>) {
    let mut present = Vec::new();
    let mut warnings = Vec::new();
    for att in attachments {
        if blobs_dir
            .join(crate::blobs::sha256_hex(&att.data))
            .is_file()
        {
            present.push(att);
        } else {
            warnings.push(format!(
                "attachment '{}' blob missing from archive",
                att.filename
            ));
        }
    }
    (present, warnings)
}

/// Repair a matched row that a previous partial import (or a sync without
/// raw fetch) left incomplete: backfill the raw blob and/or the attachment
/// links from the archive. `rel` is the freshly re-stored raw blob path.
async fn repair_existing_message(
    state: &AuthState,
    account_id: &str,
    existing: &ExistingMessage,
    rel: &str,
    attachments: Vec<crate::imap::ExtractedAttachment>,
    blobs_dir: &Path,
) -> Result<MessageOutcome, BackupError> {
    let db = &state.db;
    let mut repaired = false;
    let mut warnings = Vec::new();
    if !existing.has_raw {
        store::set_message_raw_blob(db, &existing.id, rel)
            .await
            .map_err(sync_err)?;
        repaired = true;
    }
    if !existing.has_attachments && !attachments.is_empty() {
        let (present, missing) = partition_attachments(attachments, blobs_dir);
        warnings = missing;
        if !present.is_empty() {
            // The row has no attachment rows, so persist_attachments' initial
            // delete is a no-op here.
            crate::sync::http::persist_attachments(
                db,
                &state.data_dir,
                account_id,
                &existing.id,
                &present,
            )
            .await
            .map_err(sync_err)?;
            repaired = true;
        }
    }
    Ok(MessageOutcome {
        inserted: false,
        repaired,
        warnings,
    })
}

/// Verify + dedupe + insert one message from its raw bytes and sidecar line.
async fn import_one_message(
    state: &AuthState,
    account_id: &str,
    folder_id: &str,
    raw: &[u8],
    meta: &MetaLine,
    blobs_dir: &Path,
) -> Result<MessageOutcome, BackupError> {
    let sha = crate::blobs::sha256_hex(raw);
    if sha != meta.sha256 {
        return Err(BackupError::CorruptArchive);
    }
    let db = &state.db;
    let external_id = format!("import:{sha}");

    // Store the raw blob BEFORE any DB write: blobs::store is
    // content-addressed and idempotent, so a failure after this point can
    // never strand a message row whose blob is unrecoverable.
    let rel = crate::blobs::store(&state.data_dir, account_id, raw)
        .await
        .map_err(|e| BackupError::Internal(e.to_string()))?;

    let existing =
        find_existing_message(db, account_id, &external_id, meta.message_id.as_deref()).await?;

    // Re-parse headers via the lenient mail-parser path used by IMAP sync.
    // mail-parser's offset slices carry the leading space + trailing CRLF of
    // the raw header line; parse_header_metadata trims the identity/address
    // fields (raw_text/addr_text) but NOT the subject — trim everything here
    // so imported rows are clean.
    let parsed = crate::imap::parse_header_metadata(raw);
    let trim = |v: Option<String>| v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let parsed_mid = trim(parsed.message_id);
    let subject = trim(parsed.subject);
    let from = trim(parsed.from);
    let to = trim(parsed.to);
    let cc = trim(parsed.cc);
    let parsed_date = trim(parsed.date);
    let in_reply_to = trim(parsed.in_reply_to);
    let references = trim(parsed.references);
    let mailer = trim(parsed.mailer);
    let (body_text, body_html_raw, attachments) = crate::imap::extract_mime_parts(raw);
    let body_html = crate::sanitize::persist_body_html(body_html_raw.as_deref());

    // Matched row: repair what it lacks instead of blindly skipping, so a
    // previously partial import heals instead of being skipped forever.
    if let Some(existing) = existing {
        return repair_existing_message(state, account_id, &existing, &rel, attachments, blobs_dir)
            .await;
    }

    let message_id_header = meta.message_id.as_deref().or(parsed_mid.as_deref());
    let date = meta.date.as_deref().or(parsed_date.as_deref());
    let is_read = meta.flags.iter().any(|f| f == "seen");
    let is_starred = meta.flags.iter().any(|f| f == "flagged");
    let from_json = from.as_ref().map(|f| json!({ "raw": f }).to_string());
    let to_json = to.as_ref().map(|t| json!(vec![t]).to_string());
    let flags_json = serde_json::to_string(&meta.flags)?;
    let snippet = subject.as_deref().map(store::truncate_for_snippet);

    // Skip-if-exists was checked above; this is a plain insert, never the
    // sync fill-in (which would refresh read/star on existing rows).
    let id = store::new_uuid_text();
    let insert = store::message_insert(
        db,
        store::MessageInsert {
            id_bind: store::id_value(db, &id).map_err(sync_err)?,
            account_bind: store::id_value(db, account_id).map_err(sync_err)?,
            folder_bind: store::id_value(db, folder_id).map_err(sync_err)?,
            external_id: &external_id,
            message_id_header,
            subject: subject.as_deref(),
            from_json: from_json.as_deref(),
            to_json: to_json.as_deref(),
            cc_json: cc.as_deref(),
            date,
            is_read,
            is_starred,
            flags_json: &flags_json,
            size_bytes: Some(i32::try_from(raw.len()).unwrap_or(i32::MAX)),
            in_reply_to: in_reply_to.as_deref(),
            references_headers: references.as_deref(),
            mailer: mailer.as_deref(),
            snippet: snippet.as_deref(),
            has_attachments: !attachments.is_empty(),
            body_text: body_text.as_deref(),
            body_html: body_html.as_deref(),
            jmap_thread_id: None,
        },
    );
    db.orm().execute(&insert).await.map_err(orm_err)?;

    store::set_message_raw_blob(db, &id, &rel)
        .await
        .map_err(sync_err)?;

    // Attachments: re-extract from the raw bytes, but only link blobs the
    // archive actually carries — a missing one is a warning, never fatal.
    let (present, warnings) = partition_attachments(attachments, blobs_dir);
    if !present.is_empty() {
        crate::sync::http::persist_attachments(db, &state.data_dir, account_id, &id, &present)
            .await
            .map_err(sync_err)?;
    }
    Ok(MessageOutcome {
        inserted: true,
        repaired: false,
        warnings,
    })
}

/// Merge one folder's mbox + sidecar. Count mismatch is recorded and the
/// min-length prefix is processed; per-message failures never abort.
async fn merge_folder_mail(
    state: &AuthState,
    account_id: &str,
    folder_id: &str,
    mail_dir: &Path,
    folder_uuid: &str,
    blobs_dir: &Path,
    report: &mut ImportReport,
) {
    let mbox_path = mail_dir.join(format!("{folder_uuid}.mbox"));
    let mbox = match tokio::fs::read(&mbox_path).await {
        Ok(bytes) => bytes,
        Err(e) => {
            report.error(format!("mail: {folder_uuid}: cannot read mbox: {e}"));
            report.messages.failed += 1;
            return;
        }
    };
    let meta_path = mail_dir.join(format!("{folder_uuid}.meta.jsonl"));
    let meta_text = match tokio::fs::read_to_string(&meta_path).await {
        Ok(text) => text,
        Err(e) => {
            report.error(format!(
                "mail: {folder_uuid}: cannot read meta sidecar: {e}"
            ));
            report.messages.failed += 1;
            return;
        }
    };
    let metas: Vec<Option<MetaLine>> = meta_text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| match serde_json::from_str(l) {
            Ok(m) => Some(m),
            Err(e) => {
                report.error(format!("mail: {folder_uuid}: bad meta line: {e}"));
                None
            }
        })
        .collect();
    let chunks = split_mbox(&mbox);
    if chunks.len() != metas.len() {
        report.error(format!(
            "mail: {folder_uuid}: mbox/meta count mismatch ({} messages vs {} sidecar lines)",
            chunks.len(),
            metas.len()
        ));
        report.messages.failed += chunks.len().abs_diff(metas.len()) as u64;
    }
    for (chunk, meta) in chunks.iter().zip(metas.iter()) {
        let Some(meta) = meta else {
            report.messages.failed += 1;
            continue;
        };
        let raw = recover_raw(chunk);
        match import_one_message(state, account_id, folder_id, &raw, meta, blobs_dir).await {
            Ok(outcome) => {
                if outcome.inserted {
                    report.messages.inserted += 1;
                } else if outcome.repaired {
                    report.messages.repaired += 1;
                } else {
                    report.messages.skipped += 1;
                }
                for w in outcome.warnings {
                    report.error(format!("mail: {folder_uuid}: {w}"));
                }
            }
            Err(BackupError::CorruptArchive) => {
                report.messages.failed += 1;
                report.error(format!(
                    "mail: {folder_uuid}: sha256 mismatch, message skipped"
                ));
            }
            Err(e) => {
                report.messages.failed += 1;
                report.error(format!("mail: {folder_uuid}: {e}"));
            }
        }
    }
}

/// Merge `mail/<n>/` for every mapped account. Accounts with an id_map entry
/// but no mail dir (or a mail dir with no mapped account) are recorded and
/// skipped.
async fn merge_messages(
    state: &AuthState,
    job_id: &str,
    id_map: &HashMap<usize, String>,
    folder_maps: &HashMap<usize, HashMap<String, String>>,
    dir: &Path,
    report: &mut ImportReport,
) {
    progress(state, job_id, "messages").await;
    let mail_root = dir.join("mail");
    let blobs_dir = dir.join("blobs");

    let mut mail_indexes: Vec<usize> = Vec::new();
    if mail_root.is_dir() {
        let mut rd = match tokio::fs::read_dir(&mail_root).await {
            Ok(rd) => rd,
            Err(e) => {
                report.error(format!("mail: cannot list mail dir: {e}"));
                return;
            }
        };
        while let Ok(Some(entry)) = rd.next_entry().await {
            if let Some(n) = entry
                .file_name()
                .to_str()
                .and_then(|name| name.parse::<usize>().ok())
            {
                mail_indexes.push(n);
            }
        }
        mail_indexes.sort_unstable();
    }

    for &n in &mail_indexes {
        let Some(account_id) = id_map.get(&n) else {
            report.error(format!("mail: account {n}: not present in id map, skipped"));
            continue;
        };
        let Some(folder_map) = folder_maps.get(&n) else {
            continue;
        };
        let mail_dir = mail_root.join(n.to_string());
        let mut mboxes: Vec<String> = Vec::new();
        let mut rd = match tokio::fs::read_dir(&mail_dir).await {
            Ok(rd) => rd,
            Err(e) => {
                report.error(format!("mail: account {n}: {e}"));
                continue;
            }
        };
        while let Ok(Some(entry)) = rd.next_entry().await {
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Some(uuid) = name.strip_suffix(".mbox") {
                mboxes.push(uuid.to_string());
            }
        }
        mboxes.sort();
        for folder_uuid in mboxes {
            let Some(folder_id) = folder_map.get(&folder_uuid) else {
                report.error(format!(
                    "mail: account {n}: folder {folder_uuid} not mapped, skipped"
                ));
                continue;
            };
            merge_folder_mail(
                state,
                account_id,
                folder_id,
                &mail_dir,
                &folder_uuid,
                &blobs_dir,
                report,
            )
            .await;
        }
    }
    // Mapped accounts whose archive account had no mail section at all.
    for n in id_map.keys() {
        if !mail_indexes.contains(n) {
            let mail_dir = mail_root.join(n.to_string());
            if !mail_dir.is_dir() {
                report.error(format!(
                    "mail: account {n}: no mail dir in archive, skipped"
                ));
            }
        }
    }
}

// ── Contacts & calendars ─────────────────────────────────────────────

/// One unfolded-ish vCard/iCal property value (`NAME` or `NAME;params:`).
/// Folding (continuation lines) is not unwound — UID/FN/SUMMARY are short.
fn card_prop<'a>(card: &'a str, name: &str) -> Option<&'a str> {
    card.lines().find_map(|line| {
        let line = line.trim_end_matches('\r');
        let (lhs, value) = line.split_once(':')?;
        let prop = lhs.split(';').next()?;
        if prop.eq_ignore_ascii_case(name) {
            Some(value.trim())
        } else {
            None
        }
    })
}

/// All values of a repeated property (EMAIL).
fn card_props<'a>(card: &'a str, name: &str) -> Vec<&'a str> {
    card.lines()
        .filter_map(|line| {
            let line = line.trim_end_matches('\r');
            let (lhs, value) = line.split_once(':')?;
            let prop = lhs.split(';').next()?;
            prop.eq_ignore_ascii_case(name).then(|| value.trim())
        })
        .filter(|v| !v.is_empty())
        .collect()
}

/// Split a concatenated vCard/iCal stream into `BEGIN:<kind>` … `END:<kind>`
/// blocks (inclusive), CRLF-normalized.
fn split_cards(text: &str, kind: &str) -> Vec<String> {
    let begin = format!("BEGIN:{kind}");
    let end = format!("END:{kind}");
    let mut out = Vec::new();
    let mut cur: Option<String> = None;
    for line in text.lines() {
        let bare = line.trim_end_matches('\r');
        if bare.eq_ignore_ascii_case(&begin) {
            cur = Some(String::new());
        }
        if let Some(c) = cur.as_mut() {
            c.push_str(bare);
            c.push_str("\r\n");
        }
        if bare.eq_ignore_ascii_case(&end)
            && let Some(c) = cur.take()
        {
            out.push(c);
        }
    }
    out
}

/// Dedupe key for one contact/event: its UID, else a content hash.
fn card_key(card: &str) -> String {
    card_prop(card, "UID").map_or_else(
        || format!("sha256:{}", crate::blobs::sha256_hex(card.as_bytes())),
        str::to_owned,
    )
}

/// The account a PIM section belongs to: first mapped account (ascending
/// archive index) whose LOCAL row carries the given DAV url, else the first
/// mapped account.
async fn pick_pim_account(
    db: &DbPool,
    id_map: &HashMap<usize, String>,
    url_col: mail_account::Column,
    url_name: &str,
) -> Result<Option<String>, BackupError> {
    let mut ordered: Vec<(usize, &String)> = id_map.iter().map(|(n, id)| (*n, id)).collect();
    ordered.sort_unstable();
    if ordered.is_empty() {
        return Ok(None);
    }
    let binds = ordered
        .iter()
        .map(|(_, id)| id_bind(db, id))
        .collect::<Result<Vec<_>, _>>()?;
    let mut sel = Sq::select();
    sel.column(mail_account::Column::Id)
        .column(url_col)
        .from(mail_account::Entity)
        .and_where(mail_account::Column::Id.is_in(binds));
    let rows = db.orm().query_all(&sel).await.map_err(orm_err)?;
    let with_dav: std::collections::HashSet<String> = rows
        .iter()
        .filter(|row| {
            row.try_get::<Option<String>>("", url_name)
                .ok()
                .flatten()
                .is_some()
        })
        .filter_map(|row| row_id(row, "id").ok())
        .collect();
    for (_, id) in &ordered {
        if with_dav.contains(*id) {
            return Ok(Some((*id).clone()));
        }
    }
    Ok(ordered.first().map(|(_, id)| (*id).clone()))
}

/// contacts.vcf → per-account additive merge keyed by vCard UID.
async fn merge_contacts(
    state: &AuthState,
    job_id: &str,
    id_map: &HashMap<usize, String>,
    dir: &Path,
    report: &mut ImportReport,
) {
    progress(state, job_id, "contacts").await;
    let Ok(text) = tokio::fs::read_to_string(dir.join("contacts.vcf")).await else {
        return;
    };
    let cards = split_cards(&text, "VCARD");
    if cards.is_empty() {
        return;
    }
    let db = &state.db;
    let account_id =
        match pick_pim_account(db, id_map, mail_account::Column::CarddavUrl, "carddav_url").await {
            Ok(Some(id)) => id,
            Ok(None) => {
                report.error("contacts: no account to attach imported contacts to".to_string());
                report.contacts.failed += cards.len() as u64;
                return;
            }
            Err(e) => {
                report.error(format!("contacts: {e}"));
                report.contacts.failed += cards.len() as u64;
                return;
            }
        };
    let existing_keys: std::collections::HashSet<String> =
        match existing_contact_keys(db, &account_id).await {
            Ok(keys) => keys,
            Err(e) => {
                report.error(format!("contacts: {e}"));
                report.contacts.failed += cards.len() as u64;
                return;
            }
        };
    for card in &cards {
        let key = card_key(card);
        if existing_keys.contains(&key) {
            report.contacts.skipped += 1;
            continue;
        }
        match insert_contact(db, &account_id, &key, card).await {
            Ok(()) => report.contacts.inserted += 1,
            Err(e) => {
                report.contacts.failed += 1;
                report.error(format!("contacts: {e}"));
            }
        }
    }
}

/// Insert one contact row from its vCard text (display name + emails parsed
/// out of the card; `external_id` carries the import dedupe key).
async fn insert_contact(
    db: &DbPool,
    account_id: &str,
    key: &str,
    card: &str,
) -> Result<(), BackupError> {
    let emails: Vec<&str> = card_props(card, "EMAIL");
    let email_json = (!emails.is_empty()).then(|| json!(emails).to_string());
    // jsonb on Postgres: bind via the dialect-aware JSON helper.
    let email_bind = store::opt_json_value(db, email_json.as_deref());
    let mut ins = Sq::insert();
    ins.into_table(contact::Entity)
        .columns([
            contact::Column::Id,
            contact::Column::AccountId,
            contact::Column::ExternalId,
            contact::Column::VcardBlob,
            contact::Column::DisplayName,
            contact::Column::EmailAddresses,
        ])
        .values_panic([
            Expr::val(id_bind(db, &store::new_uuid_text())?),
            Expr::val(id_bind(db, account_id)?),
            Expr::val(format!("import:{key}")),
            Expr::val(card.to_owned()),
            Expr::val(card_prop(card, "FN").map(str::to_owned)),
            Expr::val(email_bind),
        ]);
    db.orm().execute(&ins).await.map_err(orm_err)?;
    Ok(())
}

/// UID keys of every existing contact of the account.
async fn existing_contact_keys(
    db: &DbPool,
    account_id: &str,
) -> Result<std::collections::HashSet<String>, BackupError> {
    let mut sel = Sq::select();
    sel.column(contact::Column::VcardBlob)
        .from(contact::Entity)
        .and_where(contact::Column::AccountId.eq(id_bind(db, account_id)?));
    let rows = db.orm().query_all(&sel).await.map_err(orm_err)?;
    Ok(rows
        .iter()
        .filter_map(|row| {
            row.try_get::<Option<String>>("", "vcard_blob")
                .ok()
                .flatten()
        })
        .map(|blob| card_key(&blob))
        .collect())
}

/// UID keys of every existing event of one calendar.
async fn existing_event_keys(
    db: &DbPool,
    calendar_id: &str,
) -> Result<std::collections::HashSet<String>, BackupError> {
    let mut sel = Sq::select();
    sel.column(calendar_event::Column::IcalendarBlob)
        .from(calendar_event::Entity)
        .and_where(calendar_event::Column::CalendarId.eq(id_bind(db, calendar_id)?));
    let rows = db.orm().query_all(&sel).await.map_err(orm_err)?;
    Ok(rows
        .iter()
        .filter_map(|row| {
            row.try_get::<Option<String>>("", "icalendar_blob")
                .ok()
                .flatten()
        })
        .map(|blob| card_key(&blob))
        .collect())
}

/// calendars.json + calendars/<n>.ics: recreate calendars by name match,
/// then merge VEVENTs by UID.
async fn merge_calendars(
    state: &AuthState,
    job_id: &str,
    id_map: &HashMap<usize, String>,
    dir: &Path,
    report: &mut ImportReport,
) {
    progress(state, job_id, "calendars").await;
    let Ok(text) = tokio::fs::read_to_string(dir.join("calendars.json")).await else {
        return;
    };
    let Ok(Json::Array(entries)) = serde_json::from_str(&text) else {
        report.error("calendars.json: invalid json".to_string());
        return;
    };
    if entries.is_empty() {
        return;
    }
    let db = &state.db;
    let account_id =
        match pick_pim_account(db, id_map, mail_account::Column::CaldavUrl, "caldav_url").await {
            Ok(Some(id)) => id,
            Ok(None) => {
                report.error("calendars: no account to attach imported calendars to".to_string());
                report.calendars.failed += entries.len() as u64;
                return;
            }
            Err(e) => {
                report.error(format!("calendars: {e}"));
                report.calendars.failed += entries.len() as u64;
                return;
            }
        };

    // Existing calendars of the account, name-keyed.
    let account_bind = match id_bind(db, &account_id) {
        Ok(v) => v,
        Err(e) => {
            report.error(format!("calendars: {e}"));
            report.calendars.failed += entries.len() as u64;
            return;
        }
    };
    let mut sel = Sq::select();
    sel.columns([calendar::Column::Id, calendar::Column::Name])
        .from(calendar::Entity)
        .and_where(calendar::Column::AccountId.eq(account_bind));
    let existing_calendars: Vec<(String, String)> = match db.orm().query_all(&sel).await {
        Ok(rows) => rows
            .iter()
            .filter_map(|row| {
                let id = row_id(row, "id").ok()?;
                let name = row.try_get::<String>("", "name").ok()?;
                Some((id, name))
            })
            .collect(),
        Err(e) => {
            report.error(format!("calendars: {}", orm_err(e)));
            report.calendars.failed += entries.len() as u64;
            return;
        }
    };

    for entry in &entries {
        let Some(index) = entry.get("index").and_then(Json::as_u64) else {
            report.error("calendars.json: entry without index".to_string());
            report.calendars.failed += 1;
            continue;
        };
        let name = entry
            .get("name")
            .and_then(Json::as_str)
            .unwrap_or("(unnamed)");
        let calendar_id = match existing_calendars.iter().find(|(_, n)| n == name) {
            Some((id, _)) => {
                report.calendars.skipped += 1;
                id.clone()
            }
            None => match insert_calendar(db, &account_id, index, entry).await {
                Ok(id) => {
                    report.calendars.inserted += 1;
                    id
                }
                Err(e) => {
                    report.calendars.failed += 1;
                    report.error(format!("calendars: {e}"));
                    continue;
                }
            },
        };

        merge_calendar_events(db, &account_id, &calendar_id, dir, index, report).await;
    }
}

/// Merge `calendars/<index>.ics` into one (matched or created) calendar:
/// VEVENTs keyed by UID, existing ones skipped.
async fn merge_calendar_events(
    db: &DbPool,
    account_id: &str,
    calendar_id: &str,
    dir: &Path,
    index: u64,
    report: &mut ImportReport,
) {
    let ics_path = dir.join("calendars").join(format!("{index}.ics"));
    let Ok(ics) = tokio::fs::read_to_string(&ics_path).await else {
        return;
    };
    let events = split_cards(&ics, "VEVENT");
    if events.is_empty() {
        return;
    }
    // Events are matched per-calendar by UID.
    let scoped = match existing_event_keys(db, calendar_id).await {
        Ok(keys) => keys,
        Err(e) => {
            report.error(format!("calendars: {e}"));
            report.calendars.failed += events.len() as u64;
            return;
        }
    };
    for event in &events {
        let key = card_key(event);
        if scoped.contains(&key) {
            report.calendars.skipped += 1;
            continue;
        }
        match insert_event(db, account_id, calendar_id, &key, event).await {
            Ok(()) => report.calendars.inserted += 1,
            Err(e) => {
                report.calendars.failed += 1;
                report.error(format!("calendars: {e}"));
            }
        }
    }
}

/// Insert one calendar row matched-by-name misses (events attach by id).
async fn insert_calendar(
    db: &DbPool,
    account_id: &str,
    index: u64,
    entry: &Json,
) -> Result<String, BackupError> {
    let id = store::new_uuid_text();
    let mut ins = Sq::insert();
    ins.into_table(calendar::Entity)
        .columns([
            calendar::Column::Id,
            calendar::Column::AccountId,
            calendar::Column::ExternalId,
            calendar::Column::Name,
            calendar::Column::Color,
            calendar::Column::Description,
            calendar::Column::Timezone,
            calendar::Column::IsActive,
        ])
        .values_panic([
            Expr::val(id_bind(db, &id)?),
            Expr::val(id_bind(db, account_id)?),
            Expr::val(format!("import:{index}")),
            Expr::val(
                entry
                    .get("name")
                    .and_then(Json::as_str)
                    .unwrap_or("(unnamed)"),
            ),
            Expr::val(entry.get("color").and_then(Json::as_str)),
            Expr::val(entry.get("description").and_then(Json::as_str)),
            Expr::val(entry.get("timezone").and_then(Json::as_str)),
            Expr::val(true),
        ]);
    db.orm().execute(&ins).await.map_err(orm_err)?;
    Ok(id)
}

/// Insert one calendar event row from its VEVENT text (summary parsed out;
/// `external_id` carries the import dedupe key).
async fn insert_event(
    db: &DbPool,
    account_id: &str,
    calendar_id: &str,
    key: &str,
    event: &str,
) -> Result<(), BackupError> {
    let mut ins = Sq::insert();
    ins.into_table(calendar_event::Entity)
        .columns([
            calendar_event::Column::Id,
            calendar_event::Column::AccountId,
            calendar_event::Column::ExternalId,
            calendar_event::Column::IcalendarBlob,
            calendar_event::Column::Summary,
            calendar_event::Column::CalendarId,
        ])
        .values_panic([
            Expr::val(id_bind(db, &store::new_uuid_text())?),
            Expr::val(id_bind(db, account_id)?),
            Expr::val(format!("import:{key}")),
            Expr::val(event.to_owned()),
            Expr::val(card_prop(event, "SUMMARY").map(str::to_owned)),
            Expr::val(id_bind(db, calendar_id)?),
        ]);
    db.orm().execute(&ins).await.map_err(orm_err)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{TEST_MASTER_KEY, install_test_master_key};
    use crate::kernel::App;
    use crate::kv::MemoryKv;
    use crate::storage::{DbPool, Storage};
    use crate::sync::store;
    use std::io::Write as _;
    use std::sync::Arc;

    pub(crate) struct Fixture {
        pub(crate) state: AuthState,
        pub(crate) db: DbPool,
        pub(crate) user_id: String,
        pub(crate) data_dir: tempfile::TempDir,
    }

    pub(crate) fn sqlite_pool(db: &DbPool) -> &sqlx::SqlitePool {
        match db {
            DbPool::Sqlite(pool) => pool,
            #[cfg(feature = "mysql")]
            DbPool::Mysql(_) => panic!("expected sqlite"),
            #[cfg(feature = "postgres")]
            DbPool::Postgres(_) => panic!("expected sqlite in tests"),
        }
    }

    pub(crate) fn test_config(data_dir: &tempfile::TempDir) -> crate::config::Config {
        crate::config::Config {
            listen_addr: "127.0.0.1:0".into(),
            database_url: "sqlite::memory:".into(),
            data_dir: data_dir.path().to_string_lossy().into_owned(),
            min_password_length: 8,
            sync_max_concurrent: 3,
            sync_poll_secs: 300,
            max_attachment_bytes: 25 * 1024 * 1024,
            redis_url: None,
            otel_endpoint: None,
            otel_headers: None,
            otel_sample_ratio: 1.0,
            otel_service_name: "lyra-backend".into(),
            sentry_dsn: None,
            sentry_frontend_dsn: None,
            sentry_traces_sample_rate: 0.0,
            master_key: TEST_MASTER_KEY.to_vec(),
            ms_oauth: None,
            yandex_oauth: None,
            captcha: crate::config::CaptchaConfig::None,
            vapid_subject: "mailto:test@example.com".to_string(),
        }
    }

    /// A fresh instance: migrated in-memory SQLite, one user with a
    /// wrapped DEK, empty data dir.
    pub(crate) async fn seed_instance(username: &str) -> Fixture {
        install_test_master_key();
        let data_dir = tempfile::tempdir().unwrap();
        let storage = Storage::new("sqlite::memory:").await.unwrap();
        storage.run_migrations().await.unwrap();
        let db = storage.pool().clone();
        let config = test_config(&data_dir);
        let state = AuthState::new(
            db.clone(),
            &config,
            Arc::new(App::new()),
            Arc::new(MemoryKv::new()),
        )
        .unwrap();
        let pool = sqlite_pool(&db).clone();

        let user_id = store::new_uuid_text();
        sqlx::query("INSERT INTO lyra_user (id, username, password_hash) VALUES (?, ?, 'hash')")
            .bind(&user_id)
            .bind(username)
            .execute(&pool)
            .await
            .unwrap();
        let dek = crate::crypto::generate_key();
        let kek = crate::crypto::derive_user_kek(TEST_MASTER_KEY, &user_id);
        let wrapped = crate::crypto::wrap_dek(&kek, &dek).unwrap();
        sqlx::query("UPDATE lyra_user SET encrypted_dek = ? WHERE id = ?")
            .bind(&wrapped)
            .bind(&user_id)
            .execute(&pool)
            .await
            .unwrap();
        Fixture {
            state,
            db,
            user_id,
            data_dir,
        }
    }

    /// Copy an exported `.lyra` into `fx`'s staging as a finished upload;
    /// returns the upload id.
    pub(crate) async fn stage_upload(fx: &Fixture, artifact: &Path) -> String {
        let upload_id = Uuid::now_v7().to_string();
        let staging = fx.data_dir.path().join("backups").join("staging");
        tokio::fs::create_dir_all(&staging).await.unwrap();
        tokio::fs::copy(artifact, staging.join(format!("upload-{upload_id}.lyra")))
            .await
            .unwrap();
        upload_id
    }

    pub(crate) async fn kv_report(fx: &Fixture, job_id: &str) -> Json {
        serde_json::from_str(
            &fx.state
                .kv()
                .get(&format!("backup:report:{job_id}"))
                .await
                .unwrap()
                .unwrap(),
        )
        .unwrap()
    }

    /// Hand-build a `.lyra` from raw zip entries (already age-encrypted).
    pub(crate) fn build_archive(
        dir: &tempfile::TempDir,
        entries: &[(&str, &[u8])],
        password: &str,
    ) -> PathBuf {
        let zip_path = dir.path().join("test.zip");
        {
            let file = std::fs::File::create(&zip_path).unwrap();
            let mut zip = zip::ZipWriter::new(file);
            let options = zip::write::SimpleFileOptions::default();
            for (name, bytes) in entries {
                zip.start_file(name, options).unwrap();
                zip.write_all(bytes).unwrap();
            }
            zip.finish().unwrap();
        }
        let artifact = dir.path().join("test.lyra");
        crypto::encrypt_file(&zip_path, &artifact, password).unwrap();
        artifact
    }

    pub(crate) const GOOD_MANIFEST: &str = r#"{"format":1,"app":"lyra","app_version":"0.1.0","created_at":"2026-09-09T00:00:00Z","sections":{"settings":false,"accounts":0,"messages":0,"contacts":0,"calendars":0,"blobs":0}}"#;

    /// A real archive produced by the export writer path opens cleanly.
    #[tokio::test]
    async fn run_opens_exported_archive() {
        let src = seed_instance("export-src").await;
        let pool = sqlite_pool(&src.db).clone();
        let account_id = store::new_uuid_text();
        let dek = AuthState::get_user_dek(&src.db, &src.user_id)
            .await
            .unwrap();
        let credential = serde_json::to_string(
            &crate::crypto::encrypt(&dek, br#"{"username":"u@example.com","password":"s"}"#)
                .unwrap(),
        )
        .unwrap();
        sqlx::query(
            "INSERT INTO mail_account (id, user_id, email_address, protocol, auth_type, credential, \
             is_active, sync_enabled, receive_protocol, send_protocol) \
             VALUES (?, ?, 'u@example.com', 'imap', 'password', ?, 1, 1, 'imap', 'smtp')",
        )
        .bind(&account_id)
        .bind(&src.user_id)
        .bind(&credential)
        .execute(&pool)
        .await
        .unwrap();

        let job_id = store::new_uuid_text();
        let artifact_id = store::new_uuid_text();
        crate::backup::export::run(
            &src.state,
            &src.user_id,
            &job_id,
            &artifact_id,
            "test-password-11",
        )
        .await
        .unwrap();
        let artifact =
            crate::backup::artifacts::artifact_path(src.data_dir.path(), &artifact_id).unwrap();

        let dst = seed_instance("import-dst").await;
        let upload_id = stage_upload(&dst, &artifact).await;
        let job_id = store::new_uuid_text();
        run(
            &dst.state,
            &dst.user_id,
            &job_id,
            &upload_id,
            "test-password-11",
        )
        .await
        .unwrap();

        let report = kv_report(&dst, &job_id).await;
        assert_eq!(report["ok"], json!(true));
        // Staging is fully cleaned: upload file and extraction dir are gone.
        let leftovers: Vec<_> = std::fs::read_dir(dst.data_dir.path().join("backups/staging"))
            .unwrap()
            .collect();
        assert!(
            leftovers.is_empty(),
            "staging must be cleaned: {leftovers:?}"
        );
    }

    #[tokio::test]
    async fn tampered_manifest_is_unsupported_format() {
        let dir = tempfile::tempdir().unwrap();
        let manifest = GOOD_MANIFEST.replace(r#""format":1"#, r#""format":99"#);
        let artifact = build_archive(
            &dir,
            &[("manifest.json", manifest.as_bytes())],
            "test-password-11",
        );
        let fx = seed_instance("import-tampered").await;
        let upload_id = stage_upload(&fx, &artifact).await;
        let job_id = store::new_uuid_text();
        let err = run(
            &fx.state,
            &fx.user_id,
            &job_id,
            &upload_id,
            "test-password-11",
        )
        .await
        .unwrap_err();
        assert!(matches!(err, BackupError::UnsupportedFormat), "{err:?}");
        let report = kv_report(&fx, &job_id).await;
        assert_eq!(report["ok"], json!(false));
    }

    #[tokio::test]
    async fn zip_slip_entry_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let artifact = build_archive(
            &dir,
            &[
                ("manifest.json", GOOD_MANIFEST.as_bytes()),
                ("../evil", b"nope"),
            ],
            "test-password-11",
        );
        let fx = seed_instance("import-zipslip").await;
        let upload_id = stage_upload(&fx, &artifact).await;
        let job_id = store::new_uuid_text();
        let err = run(
            &fx.state,
            &fx.user_id,
            &job_id,
            &upload_id,
            "test-password-11",
        )
        .await
        .unwrap_err();
        assert!(matches!(err, BackupError::CorruptArchive), "{err:?}");
        assert!(!fx.data_dir.path().join("backups/evil").exists());
    }

    #[tokio::test]
    async fn wrong_password_is_invalid_password() {
        let dir = tempfile::tempdir().unwrap();
        let artifact = build_archive(
            &dir,
            &[("manifest.json", GOOD_MANIFEST.as_bytes())],
            "right-password",
        );
        let fx = seed_instance("import-wrongpw").await;
        let upload_id = stage_upload(&fx, &artifact).await;
        let job_id = store::new_uuid_text();
        let err = run(
            &fx.state,
            &fx.user_id,
            &job_id,
            &upload_id,
            "wrong-password",
        )
        .await
        .unwrap_err();
        assert!(matches!(err, BackupError::InvalidPassword), "{err:?}");
        let report = kv_report(&fx, &job_id).await;
        assert_eq!(report["ok"], json!(false));
    }

    #[test]
    fn sanitize_entry_name_rejects_traversal() {
        assert!(sanitize_entry_name("manifest.json").is_some());
        assert!(sanitize_entry_name("mail/0/folders.json").is_some());
        assert!(sanitize_entry_name("../evil").is_none());
        assert!(sanitize_entry_name("a/../../evil").is_none());
        assert!(sanitize_entry_name("/etc/passwd").is_none());
        assert!(sanitize_entry_name("a\\..\\evil").is_none());
        assert!(sanitize_entry_name("").is_none());
    }

    // ── Task 12: settings / accounts / folders merge ────────────────

    pub(crate) const ACCOUNT_DOC: &str = r#"{
        "index": 0,
        "display_name": "Test Account",
        "email_address": "u@example.com",
        "protocol": "imap",
        "auth_type": "password",
        "imap_host": "imap.example.com",
        "imap_port": 993,
        "imap_security": "tls",
        "smtp_host": "smtp.example.com",
        "smtp_port": 465,
        "smtp_security": "tls",
        "smtp_auth_type": null,
        "signature": null,
        "carddav_url": null,
        "caldav_url": null,
        "sync_enabled": true,
        "receive_protocol": "imap",
        "send_protocol": "smtp",
        "credential": {"username": "u@example.com", "password": "secret"},
        "smtp_credential": null,
        "pim_credential": null,
        "folders": []
    }"#;

    async fn import_archive(fx: &Fixture, artifact: &Path) -> (String, Json) {
        let upload_id = stage_upload(fx, artifact).await;
        let job_id = store::new_uuid_text();
        run(
            &fx.state,
            &fx.user_id,
            &job_id,
            &upload_id,
            "test-password-12",
        )
        .await
        .unwrap();
        let report = kv_report(fx, &job_id).await;
        (job_id, report)
    }

    #[tokio::test]
    async fn import_applies_ui_state() {
        let dir = tempfile::tempdir().unwrap();
        let artifact = build_archive(
            &dir,
            &[
                ("manifest.json", GOOD_MANIFEST.as_bytes()),
                ("settings.json", br#"{"ui_state":{"theme":"dark"}}"#),
            ],
            "test-password-12",
        );
        let fx = seed_instance("import-settings").await;
        let (_job, report) = import_archive(&fx, &artifact).await;
        assert_eq!(report["ok"], json!(true));
        assert_eq!(report["report"]["settings"], json!(true));

        let ui_state: String = sqlx::query_scalar("SELECT ui_state FROM lyra_user WHERE id = ?")
            .bind(&fx.user_id)
            .fetch_one(sqlite_pool(&fx.db))
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Json>(&ui_state).unwrap(),
            json!({"theme": "dark"})
        );
    }

    #[tokio::test]
    async fn existing_account_is_matched_and_credentials_untouched() {
        let fx = seed_instance("import-existing").await;
        let pool = sqlite_pool(&fx.db).clone();
        let existing_id = store::new_uuid_text();
        // Case-insensitive email match: stored with different casing.
        sqlx::query(
            "INSERT INTO mail_account (id, user_id, email_address, protocol, auth_type, credential,              is_active, sync_enabled, receive_protocol, send_protocol) \
             VALUES (?, ?, 'U@Example.com', 'imap', 'password', 'keepme', 1, 0, 'imap', 'smtp')",
        )
        .bind(&existing_id)
        .bind(&fx.user_id)
        .execute(&pool)
        .await
        .unwrap();

        let dir = tempfile::tempdir().unwrap();
        let artifact = build_archive(
            &dir,
            &[
                ("manifest.json", GOOD_MANIFEST.as_bytes()),
                ("accounts/0.json", ACCOUNT_DOC.as_bytes()),
            ],
            "test-password-12",
        );
        let (_job, report) = import_archive(&fx, &artifact).await;
        assert_eq!(
            report["report"]["accounts"],
            json!({"inserted": 0, "skipped": 1, "repaired": 0, "failed": 0})
        );

        let rows: Vec<(String, String)> =
            sqlx::query_as("SELECT id, credential FROM mail_account WHERE user_id = ?")
                .bind(&fx.user_id)
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(rows.len(), 1, "no duplicate account");
        assert_eq!(rows[0].0, existing_id);
        assert_eq!(rows[0].1, "keepme", "credentials untouched");
    }

    #[tokio::test]
    async fn new_account_is_inserted_with_decryptable_credentials() {
        let fx = seed_instance("import-newacct").await;
        let dir = tempfile::tempdir().unwrap();
        let artifact = build_archive(
            &dir,
            &[
                ("manifest.json", GOOD_MANIFEST.as_bytes()),
                ("accounts/0.json", ACCOUNT_DOC.as_bytes()),
            ],
            "test-password-12",
        );
        let (_job, report) = import_archive(&fx, &artifact).await;
        assert_eq!(
            report["report"]["accounts"],
            json!({"inserted": 1, "skipped": 0, "repaired": 0, "failed": 0})
        );

        let pool = sqlite_pool(&fx.db).clone();
        let (account_id, last_sync_at): (String, Option<String>) = sqlx::query_as(
            "SELECT id, last_sync_at FROM mail_account WHERE user_id = ? AND email_address = 'u@example.com'",
        )
        .bind(&fx.user_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(last_sync_at.is_none());

        // Credentials roundtrip through THIS instance's user DEK.
        let (dek, credential) =
            AuthState::get_user_dek_and_credential(&fx.db, &fx.user_id, &account_id)
                .await
                .unwrap();
        let envelope: crate::crypto::EncryptedCredential =
            serde_json::from_str(&credential).unwrap();
        let plain = crate::crypto::decrypt(&dek, &envelope).unwrap();
        assert_eq!(
            serde_json::from_slice::<Json>(&plain).unwrap(),
            json!({"username": "u@example.com", "password": "secret"})
        );
    }

    #[tokio::test]
    async fn nested_folders_are_recreated_with_parent_links() {
        let fx = seed_instance("import-folders").await;
        // Children listed BEFORE their parent to exercise the fixpoint loop.
        let account = ACCOUNT_DOC.replace(
            r#""folders": []"#,
            r#""folders": [
                {"id":"aaaa-child","external_id":"INBOX/Sub","name":"Sub","parent_external_id":"INBOX","role":null,"role_override":null,"sort_order":1},
                {"id":"bbbb-inbox","external_id":"INBOX","name":"INBOX","parent_external_id":null,"role":"inbox","role_override":null,"sort_order":0}
            ]"#,
        );
        let dir = tempfile::tempdir().unwrap();
        let artifact = build_archive(
            &dir,
            &[
                ("manifest.json", GOOD_MANIFEST.as_bytes()),
                ("accounts/0.json", account.as_bytes()),
                ("mail/0/folders.json", br#"{"bbbb-inbox":{"path":"INBOX","role":"inbox"},"aaaa-child":{"path":"INBOX/Sub","role":null}}"#),
            ],
            "test-password-12",
        );
        let (_job, report) = import_archive(&fx, &artifact).await;
        assert_eq!(
            report["report"]["folders"],
            json!({"inserted": 2, "skipped": 0, "repaired": 0, "failed": 0})
        );

        let pool = sqlite_pool(&fx.db).clone();
        let inbox: (String, Option<String>) =
            sqlx::query_as("SELECT id, role FROM folder WHERE external_id = 'INBOX'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(inbox.1.as_deref(), Some("inbox"));
        let sub: (String, Option<String>) =
            sqlx::query_as("SELECT name, parent_id FROM folder WHERE external_id = 'INBOX/Sub'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(sub.0, "Sub");
        assert_eq!(sub.1.as_deref(), Some(inbox.0.as_str()));
    }

    // ── Task 13: messages / PIM merge + roundtrip ───────────────────

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
            mailer: None,
            flags: vec![],
            size: None,
            body: None,
            body_text: None,
            body_html: None,
            has_attachments: false,
            attachments: vec![],
        }
    }

    /// m1's raw RFC822: one plain-text part + one base64 attachment
    /// (`attachment-bytes`), so import re-extracts and re-links the blob.
    fn raw_with_attachment() -> Vec<u8> {
        concat!(
            "From: alice@example.com\r\n",
            "To: u@example.com\r\n",
            "Subject: raw one\r\n",
            "Message-ID: <m1@example.com>\r\n",
            "Date: Mon, 01 Sep 2026 10:00:00 +0000\r\n",
            "MIME-Version: 1.0\r\n",
            "Content-Type: multipart/mixed; boundary=BB\r\n",
            "\r\n",
            "--BB\r\n",
            "Content-Type: text/plain; charset=utf-8\r\n",
            "\r\n",
            "hello body\r\n",
            "--BB\r\n",
            "Content-Type: application/octet-stream\r\n",
            "Content-Disposition: attachment; filename=\"a.bin\"\r\n",
            "Content-Transfer-Encoding: base64\r\n",
            "\r\n",
            "YXR0YWNobWVudC1ieXRlcw==\r\n",
            "--BB--\r\n",
        )
        .as_bytes()
        .to_vec()
    }

    /// Instance A for the roundtrip: account with real DEK-encrypted
    /// credentials + DAV urls, nested folders, 3 messages (raw-backed with
    /// attachment, parsed-only, empty), contact with UID, calendar + event,
    /// ui_state. `imap_host` points at 127.0.0.1:1 so the export-time server
    /// fetch fails fast (connection refused) and falls back to
    /// reconstruction.
    async fn seed_roundtrip_source() -> (Fixture, String) {
        let a = seed_instance("roundtrip-a").await;
        let pool = sqlite_pool(&a.db).clone();
        sqlx::query("UPDATE lyra_user SET ui_state = ? WHERE id = ?")
            .bind(r#"{"theme":"dark"}"#)
            .bind(&a.user_id)
            .execute(&pool)
            .await
            .unwrap();

        let dek = AuthState::get_user_dek(&a.db, &a.user_id).await.unwrap();
        let credential = serde_json::to_string(
            &crate::crypto::encrypt(&dek, br#"{"username":"u@example.com","password":"secret"}"#)
                .unwrap(),
        )
        .unwrap();
        let account_id = store::new_uuid_text();
        sqlx::query(
            "INSERT INTO mail_account (                 id, user_id, display_name, email_address, protocol, auth_type, credential,                  imap_host, imap_port, imap_security, smtp_host, smtp_port, smtp_security,                  carddav_url, caldav_url, is_active, sync_enabled, receive_protocol, send_protocol             ) VALUES (?, ?, 'Test Account', 'u@example.com', 'imap', 'password', ?,                        '127.0.0.1', 1, 'tls', 'smtp.example.com', 465, 'tls',                        'https://dav.example.com/card', 'https://dav.example.com/cal',                        1, 1, 'imap', 'smtp')",
        )
        .bind(&account_id)
        .bind(&a.user_id)
        .bind(&credential)
        .execute(&pool)
        .await
        .unwrap();

        store::upsert_folder(&a.db, &account_id, "INBOX", None, &[])
            .await
            .unwrap();
        store::upsert_folder(&a.db, &account_id, "INBOX/Sub", Some("/"), &[])
            .await
            .unwrap();
        let folder_id = store::get_folder_id(&a.db, &account_id, "INBOX")
            .await
            .unwrap();
        seed_roundtrip_messages(&a, &account_id, &folder_id).await;
        seed_roundtrip_pim(&a, &account_id).await;
        (a, account_id)
    }

    /// Three messages: raw-backed with attachment, parsed-only, empty.
    async fn seed_roundtrip_messages(fx: &Fixture, account_id: &str, folder_id: &str) {
        let pool = sqlite_pool(&fx.db).clone();
        for uid in 1..=3u32 {
            store::upsert_message(&fx.db, account_id, folder_id, &imap_msg(uid))
                .await
                .unwrap();
        }
        // m1: raw bytes already in the blob store + attachment row.
        let raw1 = raw_with_attachment();
        let rel1 = crate::blobs::store(fx.data_dir.path(), account_id, &raw1)
            .await
            .unwrap();
        sqlx::query(
            "UPDATE message SET raw_blob_path = ?, is_read = 1, \
             date = '2026-09-01 10:00:00', message_id_header = '<m1@example.com>' \
             WHERE external_id = ?",
        )
        .bind(&rel1)
        .bind(store::imap_message_external_id(folder_id, 1))
        .execute(&pool)
        .await
        .unwrap();
        let msg1_id: String = sqlx::query_scalar("SELECT id FROM message WHERE external_id = ?")
            .bind(store::imap_message_external_id(folder_id, 1))
            .fetch_one(&pool)
            .await
            .unwrap();
        let att_rel = crate::blobs::store(fx.data_dir.path(), account_id, b"attachment-bytes")
            .await
            .unwrap();
        sqlx::query("INSERT INTO attachment (id, message_id, filename, storage_path) VALUES (?, ?, 'a.bin', ?)")
            .bind(store::new_uuid_text())
            .bind(&msg1_id)
            .bind(&att_rel)
            .execute(&pool)
            .await
            .unwrap();
        // m2: parsed bodies only → reconstructed at export time.
        sqlx::query(
            "UPDATE message SET body_html = '<p>你好</p>', subject = '你好 世界', \
             is_starred = 1, date = '2026-09-02 10:00:00', \
             message_id_header = '<m2@example.com>', \
             from_address = '{\"raw\":\"Alice <alice@example.com>\"}', \
             to_addresses = '[{\"name\":\"鲍勃\",\"email\":\"bob@example.com\"}]' \
             WHERE external_id = ?",
        )
        .bind(store::imap_message_external_id(folder_id, 2))
        .execute(&pool)
        .await
        .unwrap();
        // m3: fully empty → headers-only reconstruction.
        sqlx::query(
            "UPDATE message SET subject = NULL, from_address = NULL, to_addresses = NULL, \
             date = NULL, message_id_header = NULL, received_at = '2026-09-03 08:30:00' \
             WHERE external_id = ?",
        )
        .bind(store::imap_message_external_id(folder_id, 3))
        .execute(&pool)
        .await
        .unwrap();
    }

    /// One contact (with UID), one calendar + event (with UID).
    async fn seed_roundtrip_pim(fx: &Fixture, account_id: &str) {
        let pool = sqlite_pool(&fx.db).clone();
        sqlx::query(
            "INSERT INTO contact (id, account_id, external_id, vcard_blob, display_name) \
             VALUES (?, ?, 'c1.vcf', 'BEGIN:VCARD\r\nVERSION:4.0\r\nUID:c1\r\nFN:Ada\r\nEMAIL:ada@example.com\r\nEND:VCARD', 'Ada')",
        )
        .bind(store::new_uuid_text())
        .bind(account_id)
        .execute(&pool)
        .await
        .unwrap();

        let calendar_id = store::new_uuid_text();
        sqlx::query(
            "INSERT INTO calendar (id, account_id, external_id, name, color) \
             VALUES (?, ?, 'cal1', 'Personal', '#ff0000')",
        )
        .bind(&calendar_id)
        .bind(account_id)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO calendar_event (id, account_id, external_id, icalendar_blob, summary, calendar_id) \
             VALUES (?, ?, 'e1', 'BEGIN:VEVENT\r\nUID:e1\r\nSUMMARY:Evt\r\nEND:VEVENT', 'Evt', ?)",
        )
        .bind(store::new_uuid_text())
        .bind(account_id)
        .bind(&calendar_id)
        .execute(&pool)
        .await
        .unwrap();
    }

    async fn export_instance_a(a: &Fixture) -> PathBuf {
        let job_id = store::new_uuid_text();
        let artifact_id = store::new_uuid_text();
        crate::backup::export::run(&a.state, &a.user_id, &job_id, &artifact_id, "roundtrip-pw")
            .await
            .unwrap();
        crate::backup::artifacts::artifact_path(a.data_dir.path(), &artifact_id).unwrap()
    }

    /// (message_id_header, subject, is_read, is_starred, raw_blob_path).
    type MessageRow = (Option<String>, Option<String>, i64, i64, Option<String>);

    /// Assert the full merged state of a successful roundtrip import.
    async fn assert_roundtrip_state(b: &Fixture) {
        let pool = sqlite_pool(&b.db).clone();

        // Account present with decryptable credentials.
        let account_id: String =
            sqlx::query_scalar("SELECT id FROM mail_account WHERE email_address = 'u@example.com'")
                .fetch_one(&pool)
                .await
                .unwrap();
        let (dek, credential) =
            AuthState::get_user_dek_and_credential(&b.db, &b.user_id, &account_id)
                .await
                .unwrap();
        let envelope: crate::crypto::EncryptedCredential =
            serde_json::from_str(&credential).unwrap();
        let plain = crate::crypto::decrypt(&dek, &envelope).unwrap();
        assert_eq!(
            serde_json::from_slice::<Json>(&plain).unwrap(),
            json!({"username": "u@example.com", "password": "secret"})
        );

        assert_roundtrip_mail(b, &pool, &account_id).await;
    }

    /// Folder tree, message flags/raw blobs, and the re-linked attachment.
    async fn assert_roundtrip_mail(b: &Fixture, pool: &sqlx::SqlitePool, account_id: &str) {
        // Folder tree: INBOX with child Sub.
        let inbox: (String,) = sqlx::query_as("SELECT id FROM folder WHERE external_id = 'INBOX'")
            .fetch_one(pool)
            .await
            .unwrap();
        let sub_parent: Option<String> =
            sqlx::query_scalar("SELECT parent_id FROM folder WHERE external_id = 'INBOX/Sub'")
                .fetch_one(pool)
                .await
                .unwrap();
        assert_eq!(sub_parent.as_deref(), Some(inbox.0.as_str()));

        // Three messages, flags intact, raw blobs stored + readable.
        let messages: Vec<MessageRow> = sqlx::query_as(
            "SELECT message_id_header, subject, is_read, is_starred, raw_blob_path \
             FROM message WHERE account_id = ? ORDER BY message_id_header IS NULL, message_id_header",
        )
        .bind(account_id)
        .fetch_all(pool)
        .await
        .unwrap();
        assert_eq!(messages.len(), 3);
        let m1 = &messages[0];
        assert_eq!(m1.0.as_deref(), Some("<m1@example.com>"));
        assert_eq!(m1.1.as_deref(), Some("raw one"));
        assert_eq!(m1.2, 1, "m1 stays read");
        let m2 = &messages[1];
        assert_eq!(
            m2.1.as_deref(),
            Some("你好 世界"),
            "encoded-word subject decoded"
        );
        assert_eq!(m2.3, 1, "m2 stays starred");
        let m3 = &messages[2];
        assert!(
            m3.0.is_none() && m3.1.is_none(),
            "empty message stays empty"
        );
        for m in &messages {
            let rel = m.4.as_deref().expect("raw_blob_path set");
            let raw = crate::blobs::read(b.data_dir.path(), rel).await.unwrap();
            assert!(!raw.is_empty());
            let sha = crate::blobs::sha256_hex(&raw);
            let ext: String =
                sqlx::query_scalar("SELECT external_id FROM message WHERE raw_blob_path = ?")
                    .bind(rel)
                    .fetch_one(pool)
                    .await
                    .unwrap();
            assert_eq!(ext, format!("import:{sha}"));
        }

        // m1's attachment re-linked from the archive blob.
        let attachments: Vec<(String, String)> = sqlx::query_as(
            "SELECT a.filename, a.storage_path FROM attachment a \
             JOIN message m ON m.id = a.message_id WHERE m.account_id = ?",
        )
        .bind(account_id)
        .fetch_all(pool)
        .await
        .unwrap();
        assert_eq!(attachments.len(), 1);
        assert_eq!(attachments[0].0, "a.bin");
        let bytes = crate::blobs::read(b.data_dir.path(), &attachments[0].1)
            .await
            .unwrap();
        assert_eq!(bytes, b"attachment-bytes");

        // Contact + calendar + event present.
        let contact: String =
            sqlx::query_scalar("SELECT vcard_blob FROM contact WHERE account_id = ?")
                .bind(account_id)
                .fetch_one(pool)
                .await
                .unwrap();
        assert!(contact.contains("UID:c1"));
        let calendar: String = sqlx::query_scalar(
            "SELECT id FROM calendar WHERE name = 'Personal' AND account_id = ?",
        )
        .bind(account_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let event: String =
            sqlx::query_scalar("SELECT icalendar_blob FROM calendar_event WHERE calendar_id = ?")
                .bind(&calendar)
                .fetch_one(pool)
                .await
                .unwrap();
        assert!(event.contains("UID:e1"));

        // ui_state applied.
        let ui_state: String = sqlx::query_scalar("SELECT ui_state FROM lyra_user WHERE id = ?")
            .bind(&b.user_id)
            .fetch_one(pool)
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Json>(&ui_state).unwrap(),
            json!({"theme": "dark"})
        );
    }

    #[tokio::test]
    async fn roundtrip_export_import_then_idempotent_reimport() {
        let (a, _account_id) = seed_roundtrip_source().await;
        let artifact = export_instance_a(&a).await;

        // Fresh instance B: full import.
        let b = seed_instance("roundtrip-b").await;
        let upload1 = stage_upload(&b, &artifact).await;
        let upload2 = stage_upload(&b, &artifact).await;
        let job1 = store::new_uuid_text();
        run(&b.state, &b.user_id, &job1, &upload1, "roundtrip-pw")
            .await
            .unwrap();
        let report1 = kv_report(&b, &job1).await;
        assert_eq!(
            report1["report"]["errors"],
            json!([]),
            "unexpected errors: {report1}"
        );
        assert_eq!(report1["report"]["settings"], json!(true));
        assert_eq!(
            report1["report"]["accounts"],
            json!({"inserted": 1, "skipped": 0, "repaired": 0, "failed": 0})
        );
        assert_eq!(
            report1["report"]["folders"],
            json!({"inserted": 2, "skipped": 0, "repaired": 0, "failed": 0})
        );
        assert_eq!(
            report1["report"]["messages"],
            json!({"inserted": 3, "skipped": 0, "repaired": 0, "failed": 0})
        );
        assert_eq!(
            report1["report"]["contacts"],
            json!({"inserted": 1, "skipped": 0, "repaired": 0, "failed": 0})
        );
        assert_eq!(
            report1["report"]["calendars"],
            json!({"inserted": 2, "skipped": 0, "repaired": 0, "failed": 0})
        );
        assert_roundtrip_state(&b).await;

        // Re-import the SAME archive into B: everything skips.
        let job2 = store::new_uuid_text();
        run(&b.state, &b.user_id, &job2, &upload2, "roundtrip-pw")
            .await
            .unwrap();
        let report2 = kv_report(&b, &job2).await;
        assert_eq!(
            report2["report"]["accounts"],
            json!({"inserted": 0, "skipped": 1, "repaired": 0, "failed": 0})
        );
        assert_eq!(
            report2["report"]["folders"],
            json!({"inserted": 0, "skipped": 2, "repaired": 0, "failed": 0})
        );
        assert_eq!(
            report2["report"]["messages"],
            json!({"inserted": 0, "skipped": 3, "repaired": 0, "failed": 0})
        );
        assert_eq!(
            report2["report"]["contacts"],
            json!({"inserted": 0, "skipped": 1, "repaired": 0, "failed": 0})
        );
        assert_eq!(
            report2["report"]["calendars"],
            json!({"inserted": 0, "skipped": 2, "repaired": 0, "failed": 0})
        );
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM message")
            .fetch_one(sqlite_pool(&b.db))
            .await
            .unwrap();
        assert_eq!(count, 3, "idempotent: message count unchanged");
    }

    #[tokio::test]
    async fn import_into_instance_with_matching_account_merges_messages() {
        let (a, _account_id) = seed_roundtrip_source().await;
        let artifact = export_instance_a(&a).await;

        // Instance C already has the same account (protocol + email).
        let c = seed_instance("roundtrip-c").await;
        let pool_c = sqlite_pool(&c.db).clone();
        let existing_id = store::new_uuid_text();
        sqlx::query(
            "INSERT INTO mail_account (id, user_id, email_address, protocol, auth_type, credential, \
             is_active, sync_enabled, receive_protocol, send_protocol) \
             VALUES (?, ?, 'u@example.com', 'imap', 'password', 'keepme', 1, 1, 'imap', 'smtp')",
        )
        .bind(&existing_id)
        .bind(&c.user_id)
        .execute(&pool_c)
        .await
        .unwrap();

        let upload = stage_upload(&c, &artifact).await;
        let job = store::new_uuid_text();
        run(&c.state, &c.user_id, &job, &upload, "roundtrip-pw")
            .await
            .unwrap();
        let report = kv_report(&c, &job).await;
        assert_eq!(
            report["report"]["accounts"],
            json!({"inserted": 0, "skipped": 1, "repaired": 0, "failed": 0})
        );
        assert_eq!(
            report["report"]["messages"],
            json!({"inserted": 3, "skipped": 0, "repaired": 0, "failed": 0})
        );

        let accounts: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM mail_account")
            .fetch_one(&pool_c)
            .await
            .unwrap();
        assert_eq!(accounts, 1);
        let credential: String =
            sqlx::query_scalar("SELECT credential FROM mail_account WHERE id = ?")
                .bind(&existing_id)
                .fetch_one(&pool_c)
                .await
                .unwrap();
        assert_eq!(credential, "keepme", "existing credentials untouched");
        let merged: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM message WHERE account_id = ?")
            .bind(&existing_id)
            .fetch_one(&pool_c)
            .await
            .unwrap();
        assert_eq!(merged, 3, "messages merge into the existing account");
    }

    #[test]
    fn mbox_split_recovers_writer_bytes() {
        use crate::backup::format::write_mbox_message;
        let raws: Vec<Vec<u8>> = vec![
            b"Subject: one\r\n\r\nFrom nowhere in body\r\n>From quoted\r\n".to_vec(),
            b"Subject: two\r\n\r\nno trailing newline".to_vec(),
            b"Subject: three\r\n\r\nends with crlf\r\n".to_vec(),
        ];
        let mut mbox = Vec::new();
        for (i, raw) in raws.iter().enumerate() {
            write_mbox_message(
                &mut mbox,
                "a@b.com",
                1_700_000_000 + i64::try_from(i).unwrap(),
                raw,
            )
            .unwrap();
        }
        let chunks = split_mbox(&mbox);
        assert_eq!(chunks.len(), 3);
        for (chunk, raw) in chunks.iter().zip(raws.iter()) {
            assert_eq!(&recover_raw(chunk), raw);
        }
    }

    #[test]
    fn mbox_split_empty_is_no_messages() {
        assert!(split_mbox(b"").is_empty());
    }

    /// Two mbox messages but one sidecar line: min length processed, the
    /// excess recorded as failed, folder not aborted.
    #[tokio::test]
    async fn meta_count_mismatch_is_recorded_not_fatal() {
        use crate::backup::format::write_mbox_message;
        let folder_uuid = "f0f0f0f0-1111-4111-8111-111111111111";
        let raw1 = b"Subject: one\r\n\r\nbody one\r\n".to_vec();
        let raw2 = b"Subject: two\r\n\r\nbody two\r\n".to_vec();
        let mut mbox = Vec::new();
        write_mbox_message(&mut mbox, "a@b.com", 1, &raw1).unwrap();
        write_mbox_message(&mut mbox, "a@b.com", 2, &raw2).unwrap();
        let meta = serde_json::to_string(&MetaLine {
            message_id: None,
            flags: vec![],
            date: None,
            sha256: crate::blobs::sha256_hex(&raw1),
            reconstructed: false,
        })
        .unwrap();

        let account = ACCOUNT_DOC.replace(
            r#""folders": []"#,
            &format!(
                r#""folders": [{{"id":"{folder_uuid}","external_id":"INBOX","name":"INBOX","parent_external_id":null,"role":"inbox","role_override":null,"sort_order":0}}]"#
            ),
        );
        let folders_json =
            br#"{"f0f0f0f0-1111-4111-8111-111111111111":{"path":"INBOX","role":"inbox"}}"#;
        let mbox_name = format!("mail/0/{folder_uuid}.mbox");
        let meta_name = format!("mail/0/{folder_uuid}.meta.jsonl");
        let dir = tempfile::tempdir().unwrap();
        let artifact = build_archive(
            &dir,
            &[
                ("manifest.json", GOOD_MANIFEST.as_bytes()),
                ("accounts/0.json", account.as_bytes()),
                ("mail/0/folders.json", folders_json.as_slice()),
                (&mbox_name, &mbox),
                (&meta_name, meta.as_bytes()),
            ],
            "test-password-12",
        );
        let fx = seed_instance("import-mismatch").await;
        let (_job, report) = import_archive(&fx, &artifact).await;
        assert_eq!(
            report["report"]["messages"],
            json!({"inserted": 1, "skipped": 0, "repaired": 0, "failed": 1}),
            "{report}"
        );
        let errors = report["report"]["errors"].as_array().unwrap();
        assert!(
            errors
                .iter()
                .any(|e| e.as_str().unwrap().contains("count mismatch")),
            "{report}"
        );
    }

    /// A tampered sidecar checksum skips that message and reports it.
    #[tokio::test]
    async fn sha_mismatch_skips_message_with_error() {
        use crate::backup::format::write_mbox_message;
        let folder_uuid = "f0f0f0f0-2222-4222-8222-222222222222";
        let raw = b"Subject: x\r\n\r\nbody\r\n".to_vec();
        let mut mbox = Vec::new();
        write_mbox_message(&mut mbox, "a@b.com", 1, &raw).unwrap();
        let meta = serde_json::to_string(&MetaLine {
            message_id: None,
            flags: vec!["seen".into()],
            date: None,
            sha256: "00".repeat(32),
            reconstructed: false,
        })
        .unwrap();

        let account = ACCOUNT_DOC.replace(
            r#""folders": []"#,
            &format!(
                r#""folders": [{{"id":"{folder_uuid}","external_id":"INBOX","name":"INBOX","parent_external_id":null,"role":"inbox","role_override":null,"sort_order":0}}]"#
            ),
        );
        let mbox_name = format!("mail/0/{folder_uuid}.mbox");
        let meta_name = format!("mail/0/{folder_uuid}.meta.jsonl");
        let dir = tempfile::tempdir().unwrap();
        let artifact = build_archive(
            &dir,
            &[
                ("manifest.json", GOOD_MANIFEST.as_bytes()),
                ("accounts/0.json", account.as_bytes()),
                (&mbox_name, &mbox),
                (&meta_name, meta.as_bytes()),
            ],
            "test-password-12",
        );
        let fx = seed_instance("import-sha").await;
        let (_job, report) = import_archive(&fx, &artifact).await;
        assert_eq!(
            report["report"]["messages"],
            json!({"inserted": 0, "skipped": 0, "repaired": 0, "failed": 1}),
            "{report}"
        );
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM message")
            .fetch_one(sqlite_pool(&fx.db))
            .await
            .unwrap();
        assert_eq!(count, 0);
    }

    /// Shared body of the postgres_live / mysql_live import roundtrips:
    /// build a small archive with unique-per-run merge keys, stage it as a
    /// finished upload, run the import, return the state + report + keys.
    pub(crate) struct LiveImport {
        pub(crate) report: Json,
        pub(crate) email: String,
        pub(crate) folder_wire: String,
        pub(crate) raw: Vec<u8>,
        pub(crate) _data_dir: tempfile::TempDir,
    }

    pub(crate) async fn run_live_import(db: &DbPool, user_id: &str) -> LiveImport {
        install_test_master_key();
        let data_dir = tempfile::tempdir().unwrap();
        let config = test_config(&data_dir);
        let state = AuthState::new(
            db.clone(),
            &config,
            Arc::new(App::new()),
            Arc::new(MemoryKv::new()),
        )
        .unwrap();

        // Unique-per-run merge keys: the live database can be non-ephemeral,
        // so a second run must not collapse into skips. Use the RANDOM tail
        // segment of the v7 id — the leading hex is the epoch timestamp and
        // is constant across runs within weeks.
        let token = store::new_uuid_text()
            .rsplit('-')
            .next()
            .unwrap()
            .to_string();
        let email = format!("import-live-{token}@example.com");
        let folder_uuid = store::new_uuid_text();
        let folder_wire = format!("INBOX-LIVE-{token}");
        let raw = format!("From: a@example.com\r\nSubject: live {token}\r\n\r\nbody {token}\r\n")
            .into_bytes();
        let mut mbox = Vec::new();
        crate::backup::format::write_mbox_message(&mut mbox, "a@b.com", 1_700_000_000, &raw)
            .unwrap();
        let meta = serde_json::to_string(&MetaLine {
            message_id: Some(format!("<live-{token}@example.com>")),
            flags: vec!["seen".into()],
            date: Some("2026-09-01T10:00:00+00:00".into()),
            sha256: crate::blobs::sha256_hex(&raw),
            reconstructed: false,
        })
        .unwrap();

        let account = ACCOUNT_DOC
            .replace("u@example.com", &email)
            .replace(
                r#""carddav_url": null"#,
                r#""carddav_url": "https://dav.example.com/card""#,
            )
            .replace(
                r#""caldav_url": null"#,
                r#""caldav_url": "https://dav.example.com/cal""#,
            )
            .replace(
                r#""folders": []"#,
                &format!(
                    r#""folders": [{{"id":"{folder_uuid}","external_id":"{folder_wire}","name":"{folder_wire}","parent_external_id":null,"role":"inbox","role_override":null,"sort_order":0}}]"#
                ),
            );
        let contact_card = format!(
            "BEGIN:VCARD\r\nVERSION:4.0\r\nUID:livec-{token}\r\nFN:Live {token}\r\nEND:VCARD"
        );
        let calendars_json = format!(r#"[{{"index":0,"name":"Live {token}","color":null}}]"#);
        let ics = format!(
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:livee-{token}\r\nSUMMARY:Live {token}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
        );
        let mbox_name = format!("mail/0/{folder_uuid}.mbox");
        let meta_name = format!("mail/0/{folder_uuid}.meta.jsonl");
        let tmp = tempfile::tempdir().unwrap();
        let artifact = build_archive(
            &tmp,
            &[
                ("manifest.json", GOOD_MANIFEST.as_bytes()),
                (
                    "settings.json",
                    br#"{"ui_state":{"theme":"live"}}"#.as_slice(),
                ),
                ("accounts/0.json", account.as_bytes()),
                ("contacts.vcf", contact_card.as_bytes()),
                ("calendars.json", calendars_json.as_bytes()),
                ("calendars/0.ics", ics.as_bytes()),
                (&mbox_name, &mbox),
                (&meta_name, meta.as_bytes()),
            ],
            "live-password",
        );

        let upload_id = Uuid::now_v7().to_string();
        let staging = data_dir.path().join("backups").join("staging");
        std::fs::create_dir_all(&staging).unwrap();
        std::fs::copy(&artifact, staging.join(format!("upload-{upload_id}.lyra"))).unwrap();
        let job_id = store::new_uuid_text();
        run(&state, user_id, &job_id, &upload_id, "live-password")
            .await
            .unwrap();
        let report: Json = serde_json::from_str(
            &state
                .kv()
                .get(&format!("backup:report:{job_id}"))
                .await
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        LiveImport {
            report,
            email,
            folder_wire,
            raw,
            _data_dir: data_dir,
        }
    }

    /// A row left behind by a partial import (raw_blob_path NULL, no
    /// attachment rows) is REPAIRED on re-import — raw blob backfilled,
    /// attachments linked — and counted in the report's `repaired` bucket.
    #[tokio::test]
    async fn partial_import_row_is_repaired_not_skipped_forever() {
        use crate::backup::format::write_mbox_message;
        let folder_uuid = "f0f0f0f0-3333-4333-8333-333333333333";
        let raw = raw_with_attachment();
        let sha = crate::blobs::sha256_hex(&raw);
        let mut mbox = Vec::new();
        write_mbox_message(&mut mbox, "a@b.com", 1, &raw).unwrap();
        let meta = serde_json::to_string(&MetaLine {
            message_id: Some("<m1@example.com>".into()),
            flags: vec!["seen".into()],
            date: None,
            sha256: sha.clone(),
            reconstructed: false,
        })
        .unwrap();
        let account = ACCOUNT_DOC.replace(
            r#""folders": []"#,
            &format!(
                r#""folders": [{{"id":"{folder_uuid}","external_id":"INBOX","name":"INBOX","parent_external_id":null,"role":"inbox","role_override":null,"sort_order":0}}]"#
            ),
        );
        let att_blob = crate::blobs::sha256_hex(b"attachment-bytes");
        let mbox_name = format!("mail/0/{folder_uuid}.mbox");
        let meta_name = format!("mail/0/{folder_uuid}.meta.jsonl");
        let blob_name = format!("blobs/{att_blob}");
        let dir = tempfile::tempdir().unwrap();
        let artifact = build_archive(
            &dir,
            &[
                ("manifest.json", GOOD_MANIFEST.as_bytes()),
                ("accounts/0.json", account.as_bytes()),
                (&mbox_name, &mbox),
                (&meta_name, meta.as_bytes()),
                (&blob_name, b"attachment-bytes".as_slice()),
            ],
            "test-password-12",
        );

        let fx = seed_instance("import-repair").await;
        let pool = sqlite_pool(&fx.db).clone();
        // Pre-seed the account + folder + the PARTIAL message row (external
        // id matches, raw_blob_path NULL, no attachments) — the state a
        // transient failure after the row insert would have left behind.
        let account_id = store::new_uuid_text();
        sqlx::query(
            "INSERT INTO mail_account (id, user_id, email_address, protocol, auth_type, credential, \
             is_active, sync_enabled, receive_protocol, send_protocol) \
             VALUES (?, ?, 'u@example.com', 'imap', 'password', 'keepme', 1, 1, 'imap', 'smtp')",
        )
        .bind(&account_id)
        .bind(&fx.user_id)
        .execute(&pool)
        .await
        .unwrap();
        store::upsert_folder(&fx.db, &account_id, "INBOX", None, &[])
            .await
            .unwrap();
        let folder_id = store::get_folder_id(&fx.db, &account_id, "INBOX")
            .await
            .unwrap();
        store::upsert_message(&fx.db, &account_id, &folder_id, &imap_msg(1))
            .await
            .unwrap();
        sqlx::query(
            "UPDATE message SET external_id = ?, raw_blob_path = NULL, has_attachments = 0",
        )
        .bind(format!("import:{sha}"))
        .execute(&pool)
        .await
        .unwrap();

        let upload1 = stage_upload(&fx, &artifact).await;
        let job1 = store::new_uuid_text();
        run(&fx.state, &fx.user_id, &job1, &upload1, "test-password-12")
            .await
            .unwrap();
        let report = kv_report(&fx, &job1).await;
        assert_eq!(
            report["report"]["messages"],
            json!({"inserted": 0, "skipped": 0, "repaired": 1, "failed": 0}),
            "{report}"
        );
        assert_repaired_message(&fx, &account_id, &raw).await;

        // Once healed, the next import is a plain skip again.
        let upload2 = stage_upload(&fx, &artifact).await;
        let job2 = store::new_uuid_text();
        run(&fx.state, &fx.user_id, &job2, &upload2, "test-password-12")
            .await
            .unwrap();
        let report = kv_report(&fx, &job2).await;
        assert_eq!(
            report["report"]["messages"],
            json!({"inserted": 0, "skipped": 1, "repaired": 0, "failed": 0}),
            "{report}"
        );
    }

    /// Assert the repair fully healed the row: raw blob readable,
    /// attachment re-linked from the archive blob.
    async fn assert_repaired_message(fx: &Fixture, account_id: &str, raw: &[u8]) {
        let pool = sqlite_pool(&fx.db).clone();
        let rel: Option<String> =
            sqlx::query_scalar("SELECT raw_blob_path FROM message WHERE account_id = ?")
                .bind(account_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        let rel = rel.expect("raw blob backfilled");
        assert_eq!(
            crate::blobs::read(fx.data_dir.path(), &rel).await.unwrap(),
            raw
        );
        let attachments: Vec<(String, String)> = sqlx::query_as(
            "SELECT filename, storage_path FROM attachment a \
             JOIN message m ON m.id = a.message_id WHERE m.account_id = ?",
        )
        .bind(account_id)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(attachments.len(), 1);
        assert_eq!(attachments[0].0, "a.bin");
        assert_eq!(
            crate::blobs::read(fx.data_dir.path(), &attachments[0].1)
                .await
                .unwrap(),
            b"attachment-bytes"
        );
    }

    /// The extraction size cap stops zip bombs (tested with a tiny budget).
    #[tokio::test]
    async fn extraction_size_cap_is_enforced() {
        let dir = tempfile::tempdir().unwrap();
        let zip_path = dir.path().join("bomb.zip");
        {
            let file = std::fs::File::create(&zip_path).unwrap();
            let mut zip = zip::ZipWriter::new(file);
            let options = zip::write::SimpleFileOptions::default();
            zip.start_file("manifest.json", options).unwrap();
            zip.write_all(GOOD_MANIFEST.as_bytes()).unwrap();
            zip.start_file("big.bin", options).unwrap();
            zip.write_all(&[b'x'; 100]).unwrap();
            zip.finish().unwrap();
        }
        let out = tempfile::tempdir().unwrap();
        let err = extract_archive(&zip_path, out.path(), 10).unwrap_err();
        assert!(matches!(err, BackupError::CorruptArchive), "{err:?}");
        // Under the real budget the same archive extracts fine.
        let out2 = tempfile::tempdir().unwrap();
        extract_archive(&zip_path, out2.path(), MAX_EXTRACTED_BYTES).unwrap();
        assert_eq!(
            std::fs::read(out2.path().join("big.bin")).unwrap().len(),
            100
        );
    }
}
#[cfg(test)]
#[cfg(feature = "postgres")]
mod postgres_live {
    //! Live-PostgreSQL roundtrip for the import merge seam: every new INSERT
    //! shape (account, folder, message via `store::message_insert`, contact,
    //! calendar, event) binds native UUIDs and jsonb — SQLite's loose typing
    //! forgives the mistakes this catches. Mirrors `sync::store::postgres_live`.
    use super::tests::run_live_import;
    use super::*;
    use crate::pgtest::support;

    #[test]
    #[ignore = "needs postgres"]
    fn import_merge_roundtrip() {
        support::rt().block_on(async {
            let (db, user_id) = support::setup().await;
            let live = run_live_import(&db, &user_id).await;
            let report = &live.report;
            assert_eq!(report["ok"], json!(true), "{report}");
            assert_eq!(report["report"]["accounts"]["inserted"], json!(1), "{report}");
            assert_eq!(report["report"]["folders"]["inserted"], json!(1), "{report}");
            assert_eq!(report["report"]["messages"]["inserted"], json!(1), "{report}");
            assert_eq!(report["report"]["contacts"]["inserted"], json!(1), "{report}");
            assert_eq!(report["report"]["calendars"]["inserted"], json!(2), "{report}");
            assert_eq!(report["report"]["settings"], json!(true), "{report}");
            assert_eq!(report["report"]["errors"], json!([]), "{report}");

            let DbPool::Postgres(pool) = &db else {
                panic!("expected postgres pool")
            };
            let token = live
                .email
                .strip_prefix("import-live-")
                .and_then(|r| r.strip_suffix("@example.com"))
                .unwrap()
                .to_string();

            // Account with credentials re-encrypted under THIS instance's DEK.
            let account_id: String =
                sqlx::query_scalar("SELECT id::text FROM mail_account WHERE email_address = $1")
                    .bind(&live.email)
                    .fetch_one(pool)
                    .await
                    .unwrap();
            let (dek, credential) =
                AuthState::get_user_dek_and_credential(&db, &user_id, &account_id)
                    .await
                    .unwrap();
            let envelope: crate::crypto::EncryptedCredential =
                serde_json::from_str(&credential).unwrap();
            let plain = crate::crypto::decrypt(&dek, &envelope).unwrap();
            assert_eq!(
                serde_json::from_slice::<Json>(&plain).unwrap(),
                json!({"username": live.email, "password": "secret"})
            );

            // Folder + message (native UUID binds, `import:` external id).
            let folder_id: String =
                sqlx::query_scalar("SELECT id::text FROM folder WHERE external_id = $1")
                    .bind(&live.folder_wire)
                    .fetch_one(pool)
                    .await
                    .unwrap();
            let (ext, is_read): (String, bool) = sqlx::query_as(
                "SELECT external_id, is_read FROM message                  WHERE account_id = $1::uuid AND folder_id = $2::uuid",
            )
            .bind(&account_id)
            .bind(&folder_id)
            .fetch_one(pool)
            .await
            .unwrap();
            assert_eq!(
                ext,
                format!("import:{}", crate::blobs::sha256_hex(&live.raw))
            );
            assert!(is_read);

            // Contact + calendar + event merged by UID.
            let contact: String =
                sqlx::query_scalar("SELECT vcard_blob FROM contact WHERE account_id = $1::uuid")
                    .bind(&account_id)
                    .fetch_one(pool)
                    .await
                    .unwrap();
            assert!(contact.contains(&format!("UID:livec-{token}")));
            let calendar_id: String =
                sqlx::query_scalar("SELECT id::text FROM calendar WHERE name = $1")
                    .bind(format!("Live {token}"))
                    .fetch_one(pool)
                    .await
                    .unwrap();
            let event: String = sqlx::query_scalar(
                "SELECT icalendar_blob FROM calendar_event WHERE calendar_id = $1::uuid",
            )
            .bind(&calendar_id)
            .fetch_one(pool)
            .await
            .unwrap();
            assert!(event.contains(&format!("UID:livee-{token}")));

            // ui_state applied on the shared pg-live user.
            let ui_state: Option<String> =
                sqlx::query_scalar("SELECT ui_state FROM lyra_user WHERE id = $1::uuid")
                    .bind(&user_id)
                    .fetch_one(pool)
                    .await
                    .unwrap();
            assert_eq!(
                serde_json::from_str::<Json>(&ui_state.unwrap()).unwrap(),
                json!({"theme": "live"})
            );
        });
    }
}

#[cfg(test)]
#[cfg(feature = "mysql")]
mod mysql_live {
    //! Live-MySQL roundtrip for the import merge seam: VARCHAR(36) id binds,
    //! JSON-as-TEXT columns, TEXT timestamps (mirrors `mytest::mysql_live`).
    use super::tests::run_live_import;
    use super::*;
    use crate::auth::{TEST_MASTER_KEY, install_test_master_key};
    use crate::mytest::support;

    /// The mysql-live user starts with the placeholder `[]` DEK; wrap a real
    /// one so the account insert can re-encrypt credentials. No other
    /// mysql_live test decrypts, so rewrapping is safe within this suite.
    async fn ensure_dek(db: &DbPool, user_id: &str) {
        install_test_master_key();
        let dek = crate::crypto::generate_key();
        let kek = crate::crypto::derive_user_kek(TEST_MASTER_KEY, user_id);
        let wrapped = crate::crypto::wrap_dek(&kek, &dek).unwrap();
        let DbPool::Mysql(pool) = db else {
            panic!("expected mysql pool")
        };
        sqlx::query("UPDATE lyra_user SET encrypted_dek = ? WHERE id = ?")
            .bind(&wrapped)
            .bind(user_id)
            .execute(pool)
            .await
            .unwrap();
    }

    #[test]
    #[ignore = "needs mysql"]
    fn import_merge_roundtrip() {
        support::rt().block_on(async {
            let (db, user_id) = support::setup().await;
            ensure_dek(&db, &user_id).await;
            let live = run_live_import(&db, &user_id).await;
            let report = &live.report;
            assert_eq!(report["ok"], json!(true), "{report}");
            assert_eq!(
                report["report"]["accounts"]["inserted"],
                json!(1),
                "{report}"
            );
            assert_eq!(
                report["report"]["messages"]["inserted"],
                json!(1),
                "{report}"
            );
            assert_eq!(
                report["report"]["contacts"]["inserted"],
                json!(1),
                "{report}"
            );
            assert_eq!(
                report["report"]["calendars"]["inserted"],
                json!(2),
                "{report}"
            );
            assert_eq!(report["report"]["errors"], json!([]), "{report}");

            let DbPool::Mysql(pool) = &db else {
                panic!("expected mysql pool")
            };
            let token = live
                .email
                .strip_prefix("import-live-")
                .and_then(|r| r.strip_suffix("@example.com"))
                .unwrap()
                .to_string();

            // TEXT ids, JSON-as-TEXT columns, tinyint flags.
            let account_id: String =
                sqlx::query_scalar("SELECT id FROM mail_account WHERE email_address = ?")
                    .bind(&live.email)
                    .fetch_one(pool)
                    .await
                    .unwrap();
            let (dek, credential) =
                AuthState::get_user_dek_and_credential(&db, &user_id, &account_id)
                    .await
                    .unwrap();
            let envelope: crate::crypto::EncryptedCredential =
                serde_json::from_str(&credential).unwrap();
            let plain = crate::crypto::decrypt(&dek, &envelope).unwrap();
            assert_eq!(
                serde_json::from_slice::<Json>(&plain).unwrap(),
                json!({"username": live.email, "password": "secret"})
            );

            let folder_id: String =
                sqlx::query_scalar("SELECT id FROM folder WHERE external_id = ?")
                    .bind(&live.folder_wire)
                    .fetch_one(pool)
                    .await
                    .unwrap();
            let (ext, is_read): (String, i8) = sqlx::query_as(
                "SELECT external_id, is_read FROM message WHERE account_id = ? AND folder_id = ?",
            )
            .bind(&account_id)
            .bind(&folder_id)
            .fetch_one(pool)
            .await
            .unwrap();
            assert_eq!(
                ext,
                format!("import:{}", crate::blobs::sha256_hex(&live.raw))
            );
            assert_eq!(is_read, 1);

            let contact: String =
                sqlx::query_scalar("SELECT vcard_blob FROM contact WHERE account_id = ?")
                    .bind(&account_id)
                    .fetch_one(pool)
                    .await
                    .unwrap();
            assert!(contact.contains(&format!("UID:livec-{token}")));
            let calendar_id: String = sqlx::query_scalar("SELECT id FROM calendar WHERE name = ?")
                .bind(format!("Live {token}"))
                .fetch_one(pool)
                .await
                .unwrap();
            let event: String = sqlx::query_scalar(
                "SELECT icalendar_blob FROM calendar_event WHERE calendar_id = ?",
            )
            .bind(&calendar_id)
            .fetch_one(pool)
            .await
            .unwrap();
            assert!(event.contains(&format!("UID:livee-{token}")));
        });
    }
}
