//! Import: decrypt the uploaded archive, validate the manifest, then merge
//! every section additively. Per-item failures are collected into the report,
//! never fatal. Staging (extracted dir + uploaded file) is removed in all
//! outcomes. Decrypted credentials and raw message bytes are never logged.
//! Spec: docs/superpowers/specs/2026-09-08-lyra-backup-export-import-design.md §6.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

use sea_orm::sea_query::{Expr, Query as Sq};
use sea_orm::{ColumnTrait, ConnectionTrait, QueryResult, Value};
use serde_json::{Value as Json, json};
use uuid::Uuid;

use crate::auth::AuthState;
use crate::entities::{folder, lyra_user, mail_account};
use crate::storage::DbPool;
use crate::sync::store;

use super::format::{FORMAT_VERSION, Manifest};
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

        let mut report = ImportReport::default();
        merge_all(state, user_id, job_id, &dir, &manifest, &mut report).await;
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
    let result = extract_archive(&zip_path, dir);
    let _ = std::fs::remove_file(&zip_path);
    result
}

/// Validate the manifest and extract every entry under `dir`, rejecting
/// zip-slip names (absolute paths, `..` components, backslashes).
fn extract_archive(zip_path: &Path, dir: &Path) -> Result<Manifest, BackupError> {
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
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(|_| BackupError::CorruptArchive)?;
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
        std::io::copy(&mut entry, &mut out)?;
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
    manifest: &Manifest,
    report: &mut ImportReport,
) {
    let _ = manifest;
    merge_settings(state, user_id, job_id, dir, report).await;
    let docs = load_account_docs(dir, report).await;
    let id_map = merge_accounts(state, user_id, job_id, &docs, report).await;
    let _folder_maps = merge_folders(state, user_id, job_id, &docs, &id_map, dir, report).await;
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
    carddav_url: Option<String>,
    caldav_url: Option<String>,
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
        mail_account::Column::CarddavUrl,
        mail_account::Column::CaldavUrl,
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
                carddav_url: row_opt_str(row, "carddav_url")?,
                caldav_url: row_opt_str(row, "caldav_url")?,
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
                    carddav_url: doc
                        .get("carddav_url")
                        .and_then(Json::as_str)
                        .map(str::to_owned),
                    caldav_url: doc
                        .get("caldav_url")
                        .and_then(Json::as_str)
                        .map(str::to_owned),
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
    fn build_archive(
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

    const GOOD_MANIFEST: &str = r#"{"format":1,"app":"lyra","app_version":"0.1.0","created_at":"2026-09-09T00:00:00Z","sections":{"settings":false,"accounts":0,"messages":0,"contacts":0,"calendars":0,"blobs":0}}"#;

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

    const ACCOUNT_DOC: &str = r#"{
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
            json!({"inserted": 0, "skipped": 1, "failed": 0})
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
            json!({"inserted": 1, "skipped": 0, "failed": 0})
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
            json!({"inserted": 2, "skipped": 0, "failed": 0})
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
}
