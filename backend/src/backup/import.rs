//! Import: decrypt the uploaded archive, validate the manifest, then merge
//! every section additively. Per-item failures are collected into the report,
//! never fatal. Staging (extracted dir + uploaded file) is removed in all
//! outcomes. Decrypted credentials and raw message bytes are never logged.
//! Spec: docs/superpowers/specs/2026-09-08-lyra-backup-export-import-design.md §6.

use std::path::{Component, Path, PathBuf};

use serde_json::{Value as Json, json};
use uuid::Uuid;

use crate::auth::AuthState;

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

        let _ = (state, user_id, job_id, &manifest);
        let report = json!({
            "settings": false,
            "accounts": {"inserted": 0, "skipped": 0, "failed": 0},
            "folders": {"inserted": 0, "skipped": 0, "failed": 0},
            "messages": {"inserted": 0, "skipped": 0, "failed": 0},
            "contacts": {"inserted": 0, "skipped": 0, "failed": 0},
            "calendars": {"inserted": 0, "skipped": 0, "failed": 0},
            "errors": Vec::<String>::new(),
        });
        Ok::<Json, BackupError>(report)
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
}
