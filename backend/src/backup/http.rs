//! HTTP surface for backup: export enqueue + job status, artifact
//! list/download/delete, and the chunked import-upload trio. All routes
//! require the bearer session (`AuthUser`); every id is scoped to the caller
//! so cross-user probes answer 404, never 403.
//! Spec: docs/superpowers/specs/2026-09-08-lyra-backup-export-import-design.md §7.

use axum::body::{Body, Bytes};
use axum::extract::{Path, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value as JsonValue, json};
use uuid::Uuid;

use crate::api_error::{ApiErrorBody, api_error, api_error_with_code};
use crate::auth::{AuthState, AuthUser};
use crate::jobs::{self, JobPayload};

use super::upload::{self, UploadError};
use super::{BackupError, artifacts};

pub(crate) fn routes() -> Router<AuthState> {
    Router::new()
        .route("/api/v1/backup/export", post(start_export))
        .route("/api/v1/backup/jobs/{job_id}", get(job_status))
        .route("/api/v1/backup/artifacts", get(list_artifacts))
        .route(
            "/api/v1/backup/artifacts/{id}/download",
            get(download_artifact),
        )
        .route("/api/v1/backup/artifacts/{id}", delete(delete_artifact))
        .route("/api/v1/backup/import/uploads", post(start_upload))
        .route(
            "/api/v1/backup/import/uploads/{id}/chunks/{n}",
            put(put_chunk),
        )
        .route(
            "/api/v1/backup/import/uploads/{id}/finish",
            post(finish_upload),
        )
}

impl IntoResponse for BackupError {
    fn into_response(self) -> Response {
        // 4xx variants are deliberate API surface and stay descriptive.
        // Everything else can carry io/zip/sql detail: log it server-side
        // and answer a generic "internal error".
        let (status, message, code) = match &self {
            BackupError::InvalidPassword => (
                StatusCode::UNPROCESSABLE_ENTITY,
                self.to_string(),
                "invalid_backup_password",
            ),
            BackupError::UnsupportedFormat => (
                StatusCode::UNPROCESSABLE_ENTITY,
                self.to_string(),
                "unsupported_backup_format",
            ),
            BackupError::CorruptArchive => (
                StatusCode::UNPROCESSABLE_ENTITY,
                self.to_string(),
                "corrupt_archive",
            ),
            BackupError::UploadIncomplete => (
                StatusCode::BAD_REQUEST,
                self.to_string(),
                "upload_incomplete",
            ),
            internal => {
                tracing::error!(error = %internal, "backup request failed");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal error".to_string(),
                    "internal_error",
                )
            }
        };
        (status, Json(ApiErrorBody::new(message, Some(code)))).into_response()
    }
}

impl IntoResponse for UploadError {
    fn into_response(self) -> Response {
        match self {
            UploadError::NotFound => api_error(StatusCode::NOT_FOUND, self.to_string()),
            UploadError::Incomplete(missing) => api_error_with_code(
                StatusCode::BAD_REQUEST,
                format!("upload incomplete; missing chunks: {missing:?}"),
                "upload_incomplete",
            ),
            UploadError::ChunkOutOfRange(_)
            | UploadError::EmptyChunk
            | UploadError::OversizedChunk => api_error(StatusCode::BAD_REQUEST, self.to_string()),
            UploadError::Io(e) => BackupError::Io(e).into_response(),
            UploadError::Backup(err) => err.into_response(),
        }
    }
}

/// Handler error type: typed 404/400 plus the store errors (pattern:
/// `push::PushHttpError`).
#[derive(Debug)]
pub(crate) enum BackupHttpError {
    NotFound(&'static str),
    BadRequest(&'static str),
    Backup(BackupError),
    Upload(UploadError),
}

impl IntoResponse for BackupHttpError {
    fn into_response(self) -> Response {
        match self {
            BackupHttpError::NotFound(m) => api_error(StatusCode::NOT_FOUND, m),
            BackupHttpError::BadRequest(m) => api_error(StatusCode::BAD_REQUEST, m),
            BackupHttpError::Backup(e) => e.into_response(),
            BackupHttpError::Upload(e) => e.into_response(),
        }
    }
}

impl From<BackupError> for BackupHttpError {
    fn from(e: BackupError) -> Self {
        BackupHttpError::Backup(e)
    }
}

impl From<UploadError> for BackupHttpError {
    fn from(e: UploadError) -> Self {
        BackupHttpError::Upload(e)
    }
}

/// Wrap the archive password for the job payload: an `EncryptedCredential`
/// JSON envelope under the user DEK — the jobs table never holds plaintext.
async fn wrap_password(
    state: &AuthState,
    user_id: &str,
    password: &str,
) -> Result<String, BackupError> {
    let dek = AuthState::get_user_dek(&state.db, user_id)
        .await
        .map_err(|e| BackupError::Crypto(e.to_string()))?;
    let envelope = crate::crypto::encrypt(&dek, password.as_bytes())
        .map_err(|e| BackupError::Crypto(e.to_string()))?;
    Ok(serde_json::to_string(&envelope)?)
}

// ── Export ─────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub(crate) struct ExportRequest {
    password: String,
}

async fn start_export(
    State(state): State<AuthState>,
    AuthUser(user_id): AuthUser,
    Json(req): Json<ExportRequest>,
) -> Result<(StatusCode, Json<JsonValue>), BackupHttpError> {
    if req.password.len() < 8 {
        return Err(BackupHttpError::BadRequest(
            "password must be at least 8 characters",
        ));
    }
    let artifact_id = Uuid::now_v7().to_string();
    let password_wrapped = wrap_password(&state, &user_id, &req.password).await?;
    let payload = JobPayload::ExportBackup {
        user_id,
        artifact_id: artifact_id.clone(),
        password_wrapped,
    };
    let job_id = jobs::enqueue(&state.db, &payload, &chrono::Utc::now().to_rfc3339())
        .await
        .map_err(BackupError::Db)?;
    // Pre-write so job_status answers "queued" before the worker picks up
    // (the job's own first write is `{"phase":"collecting"}`).
    let _ = state
        .kv()
        .set(
            &format!("backup:progress:{job_id}"),
            r#"{"phase":"queued"}"#,
            Some(3600),
        )
        .await;
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({"job_id": job_id, "artifact_id": artifact_id})),
    ))
}

// ── Job status ─────────────────────────────────────────────────────

async fn read_kv_json(state: &AuthState, key: &str) -> Result<JsonValue, BackupError> {
    match state.kv().get(key).await {
        Ok(Some(raw)) => Ok(serde_json::from_str(&raw).unwrap_or(JsonValue::Null)),
        Ok(None) => Ok(JsonValue::Null),
        Err(e) => Err(BackupError::Internal(e.to_string())),
    }
}

async fn job_status(
    State(state): State<AuthState>,
    AuthUser(user_id): AuthUser,
    Path(job_id): Path<String>,
) -> Result<Json<JsonValue>, BackupHttpError> {
    let Some(row) = jobs::job_status(&state.db, &job_id)
        .await
        .map_err(BackupError::Db)?
    else {
        return Err(BackupHttpError::NotFound("job not found"));
    };
    // Ownership: the payload must be a backup variant belonging to the
    // caller. Anything else (other user's job, non-backup kind, unparsable
    // payload) is indistinguishable from a missing row.
    let owns = match &row.payload {
        Some(
            JobPayload::ExportBackup { user_id: uid, .. }
            | JobPayload::ImportBackup { user_id: uid, .. },
        ) => uid == &user_id,
        _ => false,
    };
    if !owns {
        return Err(BackupHttpError::NotFound("job not found"));
    }
    let progress = read_kv_json(&state, &format!("backup:progress:{job_id}")).await?;
    let report = read_kv_json(&state, &format!("backup:report:{job_id}")).await?;
    Ok(Json(json!({
        "status": row.status,
        "progress": progress,
        "report": report,
    })))
}

// ── Artifacts ──────────────────────────────────────────────────────

async fn list_artifacts(
    State(state): State<AuthState>,
    AuthUser(user_id): AuthUser,
) -> Result<Json<JsonValue>, BackupHttpError> {
    let items = artifacts::list(state.kv(), &user_id).await?;
    Ok(Json(json!({"artifacts": items})))
}

async fn download_artifact(
    State(state): State<AuthState>,
    AuthUser(user_id): AuthUser,
    Path(id): Path<String>,
) -> Result<Response, BackupHttpError> {
    let items = artifacts::list(state.kv(), &user_id).await?;
    let Some(meta) = items.iter().find(|m| m.id == id) else {
        return Err(BackupHttpError::NotFound("artifact not found"));
    };
    let path = artifacts::artifact_path(&state.data_dir, &id)?;
    // User-scale files: read fully. A streaming Body::from_file variant is
    // the follow-up if archives ever outgrow memory comfort.
    let bytes = match tokio::fs::read(&path).await {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(BackupHttpError::NotFound("artifact file missing"));
        }
        Err(e) => return Err(BackupError::Io(e).into()),
    };
    // The filename is server-generated (`lyra-backup-….lyra`); strip quotes
    // and control chars anyway so it can never break the header.
    let safe_name: String = meta
        .filename
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        .collect();
    let mut res = Response::new(Body::from(bytes));
    let headers = res.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("application/octet-stream"),
    );
    headers.insert(
        header::CONTENT_DISPOSITION,
        header::HeaderValue::from_str(&format!("attachment; filename=\"{safe_name}\""))
            .map_err(|e| BackupError::Internal(e.to_string()))?,
    );
    Ok(res)
}

async fn delete_artifact(
    State(state): State<AuthState>,
    AuthUser(user_id): AuthUser,
    Path(id): Path<String>,
) -> Result<StatusCode, BackupHttpError> {
    let Some(_meta) = artifacts::remove(state.kv(), &user_id, &id).await? else {
        return Err(BackupHttpError::NotFound("artifact not found"));
    };
    if let Ok(path) = artifacts::artifact_path(&state.data_dir, &id) {
        let _ = tokio::fs::remove_file(path).await;
    }
    Ok(StatusCode::NO_CONTENT)
}

// ── Import uploads ─────────────────────────────────────────────────

async fn start_upload(
    State(state): State<AuthState>,
    AuthUser(user_id): AuthUser,
) -> Result<(StatusCode, Json<JsonValue>), BackupHttpError> {
    let upload_id = upload::start(state.kv(), &state.data_dir, &user_id).await?;
    Ok((
        StatusCode::CREATED,
        Json(json!({"upload_id": upload_id, "chunk_size": upload::CHUNK_SIZE})),
    ))
}

async fn put_chunk(
    State(state): State<AuthState>,
    AuthUser(user_id): AuthUser,
    Path((id, n)): Path<(String, u32)>,
    body: Bytes,
) -> Result<StatusCode, BackupHttpError> {
    upload::put_chunk(state.kv(), &state.data_dir, &user_id, &id, n, &body).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub(crate) struct FinishUploadRequest {
    password: String,
    total_chunks: u32,
}

async fn finish_upload(
    State(state): State<AuthState>,
    AuthUser(user_id): AuthUser,
    Path(id): Path<String>,
    Json(req): Json<FinishUploadRequest>,
) -> Result<(StatusCode, Json<JsonValue>), BackupHttpError> {
    let _staged =
        upload::finish(state.kv(), &state.data_dir, &user_id, &id, req.total_chunks).await?;
    let password_wrapped = wrap_password(&state, &user_id, &req.password).await?;
    let payload = JobPayload::ImportBackup {
        user_id,
        upload_id: id,
        password_wrapped,
    };
    let job_id = jobs::enqueue(&state.db, &payload, &chrono::Utc::now().to_rfc3339())
        .await
        .map_err(BackupError::Db)?;
    Ok((StatusCode::ACCEPTED, Json(json!({"job_id": job_id}))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use crate::kernel::App;
    use crate::kv::MemoryKv;
    use crate::storage::{DbPool, Storage};

    /// Minimal AuthState (in-memory db, temp data dir) as in jobs.rs tests.
    async fn test_state() -> (AuthState, tempfile::TempDir) {
        crate::auth::install_test_master_key();
        let storage = Storage::new("sqlite::memory:").await.unwrap();
        storage.run_migrations().await.unwrap();
        let data_dir = tempfile::tempdir().unwrap();
        let config = crate::config::Config {
            listen_addr: "127.0.0.1:0".into(),
            database_url: "sqlite::memory:".into(),
            data_dir: data_dir.path().to_string_lossy().into_owned(),
            min_password_length: 8,
            sync_max_concurrent: 3,
            sync_poll_secs: 300,
            max_attachment_bytes: 25 * 1024 * 1024,
            redis_url: None,
            master_key: crate::auth::TEST_MASTER_KEY.to_vec(),
            ms_oauth: None,
            yandex_oauth: None,
            captcha: crate::config::CaptchaConfig::None,
            vapid_subject: "mailto:test@example.com".to_string(),
        };
        let state = AuthState::new(
            storage.pool().clone(),
            &config,
            Arc::new(App::new()),
            Arc::new(MemoryKv::new()),
        )
        .unwrap();
        (state, data_dir)
    }

    #[tokio::test]
    async fn start_export_rejects_short_password() {
        let (state, _dir) = test_state().await;
        let err = start_export(
            State(state),
            AuthUser("user-1".into()),
            Json(ExportRequest {
                password: "short".into(),
            }),
        )
        .await
        .unwrap_err()
        .into_response();
        assert_eq!(err.status(), StatusCode::BAD_REQUEST);
        let body = axum::body::to_bytes(err.into_body(), 4096).await.unwrap();
        let json: JsonValue = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["code"], "bad_request");
    }

    #[tokio::test]
    async fn job_status_unknown_or_foreign_job_is_404() {
        let (state, _dir) = test_state().await;
        let db: &DbPool = &state.db;

        // Unknown id → 404.
        let err = job_status(
            State(state.clone()),
            AuthUser("user-1".into()),
            Path("nope".into()),
        )
        .await
        .unwrap_err()
        .into_response();
        assert_eq!(err.status(), StatusCode::NOT_FOUND);

        // Another user's backup job → 404 (no existence leak).
        let foreign = jobs::enqueue(
            db,
            &JobPayload::ExportBackup {
                user_id: "user-2".into(),
                artifact_id: "art".into(),
                password_wrapped: "{}".into(),
            },
            &chrono::Utc::now().to_rfc3339(),
        )
        .await
        .unwrap();
        let err = job_status(
            State(state.clone()),
            AuthUser("user-1".into()),
            Path(foreign),
        )
        .await
        .unwrap_err()
        .into_response();
        assert_eq!(err.status(), StatusCode::NOT_FOUND);

        // A non-backup job of the SAME user is also invisible here.
        let sync_job = jobs::enqueue(
            db,
            &JobPayload::SyncAccount {
                account_id: "acc".into(),
                user_id: "user-1".into(),
            },
            &chrono::Utc::now().to_rfc3339(),
        )
        .await
        .unwrap();
        let err = job_status(
            State(state.clone()),
            AuthUser("user-1".into()),
            Path(sync_job),
        )
        .await
        .unwrap_err()
        .into_response();
        assert_eq!(err.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn job_status_returns_progress_and_report() {
        let (state, _dir) = test_state().await;
        let job_id = jobs::enqueue(
            &state.db,
            &JobPayload::ExportBackup {
                user_id: "user-1".into(),
                artifact_id: "art".into(),
                password_wrapped: "{}".into(),
            },
            &chrono::Utc::now().to_rfc3339(),
        )
        .await
        .unwrap();
        state
            .kv()
            .set(
                &format!("backup:progress:{job_id}"),
                r#"{"phase":"collecting"}"#,
                Some(3600),
            )
            .await
            .unwrap();

        let Json(body) = job_status(State(state), AuthUser("user-1".into()), Path(job_id))
            .await
            .unwrap();
        assert_eq!(body["status"], "pending");
        assert_eq!(body["progress"]["phase"], "collecting");
        assert!(body["report"].is_null());
    }

    #[tokio::test]
    async fn download_and_delete_require_registry_membership() {
        let (state, dir) = test_state().await;
        let artifact_id = Uuid::now_v7().to_string();

        // A file on disk but no registry entry → 404 for download+delete.
        let path = artifacts::artifact_path(&state.data_dir, &artifact_id).unwrap();
        tokio::fs::create_dir_all(path.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(&path, b"archive-bytes").await.unwrap();
        let err = download_artifact(
            State(state.clone()),
            AuthUser("user-1".into()),
            Path(artifact_id.clone()),
        )
        .await
        .unwrap_err()
        .into_response();
        assert_eq!(err.status(), StatusCode::NOT_FOUND);

        // Register, then download serves the bytes with attachment headers.
        artifacts::add(
            state.kv(),
            "user-1",
            artifacts::ArtifactMeta {
                id: artifact_id.clone(),
                filename: "lyra-backup-20260909-120000.lyra".into(),
                size_bytes: 13,
                created_at: "2026-09-09T12:00:00Z".into(),
            },
        )
        .await
        .unwrap();
        let res = download_artifact(
            State(state.clone()),
            AuthUser("user-1".into()),
            Path(artifact_id.clone()),
        )
        .await
        .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(
            res.headers()[header::CONTENT_TYPE],
            "application/octet-stream"
        );
        assert_eq!(
            res.headers()[header::CONTENT_DISPOSITION],
            "attachment; filename=\"lyra-backup-20260909-120000.lyra\""
        );
        let body = axum::body::to_bytes(res.into_body(), 4096).await.unwrap();
        assert_eq!(&body[..], b"archive-bytes");

        // Delete removes registry entry + file; a second delete 404s.
        let status = delete_artifact(
            State(state.clone()),
            AuthUser("user-1".into()),
            Path(artifact_id.clone()),
        )
        .await
        .unwrap();
        assert_eq!(status, StatusCode::NO_CONTENT);
        assert!(!path.exists());
        let err = delete_artifact(State(state), AuthUser("user-1".into()), Path(artifact_id))
            .await
            .unwrap_err()
            .into_response();
        assert_eq!(err.status(), StatusCode::NOT_FOUND);
        let _ = dir;
    }

    #[tokio::test]
    async fn backup_error_maps_to_typed_responses() {
        let cases: [(BackupError, StatusCode, &str); 4] = [
            (
                BackupError::InvalidPassword,
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid_backup_password",
            ),
            (
                BackupError::UnsupportedFormat,
                StatusCode::UNPROCESSABLE_ENTITY,
                "unsupported_backup_format",
            ),
            (
                BackupError::CorruptArchive,
                StatusCode::UNPROCESSABLE_ENTITY,
                "corrupt_archive",
            ),
            (
                BackupError::UploadIncomplete,
                StatusCode::BAD_REQUEST,
                "upload_incomplete",
            ),
        ];
        for (err, status, code) in cases {
            let res = err.into_response();
            assert_eq!(res.status(), status);
            let body = axum::body::to_bytes(res.into_body(), 4096).await.unwrap();
            let json: JsonValue = serde_json::from_slice(&body).unwrap();
            assert_eq!(json["code"], code);
        }

        // 5xx variants log detail but answer generically.
        let res = BackupError::Internal("sql detail: secret-ish".into()).into_response();
        assert_eq!(res.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body = axum::body::to_bytes(res.into_body(), 4096).await.unwrap();
        let json: JsonValue = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["error"], "internal error");
        assert_eq!(json["code"], "internal_error");
    }
}
