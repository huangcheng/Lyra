//! OpenGPG keys HTTP API (`/api/v1/opengpg/keys`).

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
};
use serde::{Deserialize, Serialize};

use super::keys::{KeyAlgorithm, OpengpgError, generate_keypair};
use super::store::{
    StoredKey, delete_key, export_armored, export_public_armored, get_key, import_armored,
    list_keys, set_primary,
};
use crate::auth::{AuthError, AuthState, AuthUser, verify_current_password};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct KeyResponse {
    id: String,
    fingerprint: String,
    primary_email: String,
    emails: Vec<String>,
    is_secret: bool,
    is_primary: bool,
    revoked: bool,
    created_at: Option<String>,
    updated_at: Option<String>,
}

impl From<StoredKey> for KeyResponse {
    fn from(k: StoredKey) -> Self {
        Self {
            id: k.id,
            fingerprint: k.fingerprint,
            primary_email: k.primary_email,
            emails: k.emails,
            is_secret: k.is_secret,
            is_primary: k.is_primary,
            revoked: k.revoked,
            created_at: k.created_at,
            updated_at: k.updated_at,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ImportKeyRequest {
    armored: String,
    #[serde(default)]
    is_primary: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GenerateKeyRequest {
    email: String,
    #[serde(default)]
    name: String,
    passphrase: String,
    /// `rsa4096` (default) or `ed25519`.
    #[serde(default)]
    algorithm: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PatchKeyRequest {
    is_primary: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExportQuery {
    #[serde(default)]
    include_secret: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ExportResponse {
    armored: String,
    is_secret: bool,
}

impl IntoResponse for OpengpgError {
    fn into_response(self) -> axum::response::Response {
        let (status, message) = match &self {
            OpengpgError::NotFound => (StatusCode::NOT_FOUND, self.to_string()),
            OpengpgError::InvalidKey(_) | OpengpgError::MissingEmail | OpengpgError::InvalidInput(_) => {
                (StatusCode::BAD_REQUEST, self.to_string())
            }
            OpengpgError::Conflict(_) => (StatusCode::CONFLICT, self.to_string()),
            OpengpgError::Database(e) => {
                tracing::error!(error = %e, "opengpg database error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal error".to_string(),
                )
            }
        };
        (status, Json(serde_json::json!({ "error": message }))).into_response()
    }
}

pub fn routes() -> Router<AuthState> {
    Router::new()
        .route("/api/v1/opengpg/keys", get(list_keys_handler).post(import_key))
        .route("/api/v1/opengpg/keys/generate", post(generate_key))
        .route(
            "/api/v1/opengpg/keys/{id}",
            get(get_key_handler)
                .patch(patch_key)
                .delete(delete_key_handler),
        )
        .route("/api/v1/opengpg/keys/{id}/export", get(export_key))
}

async fn list_keys_handler(
    State(state): State<AuthState>,
    AuthUser(user_id): AuthUser,
) -> Result<Json<Vec<KeyResponse>>, OpengpgError> {
    let keys = list_keys(&state.db, &user_id).await?;
    Ok(Json(keys.into_iter().map(KeyResponse::from).collect()))
}

async fn get_key_handler(
    State(state): State<AuthState>,
    AuthUser(user_id): AuthUser,
    Path(id): Path<String>,
) -> Result<Json<KeyResponse>, OpengpgError> {
    let key = get_key(&state.db, &user_id, &id)
        .await?
        .ok_or(OpengpgError::NotFound)?;
    Ok(Json(KeyResponse::from(key)))
}

async fn import_key(
    State(state): State<AuthState>,
    AuthUser(user_id): AuthUser,
    Json(body): Json<ImportKeyRequest>,
) -> Result<(StatusCode, Json<KeyResponse>), OpengpgError> {
    let stored = import_armored(&state.db, &user_id, &body.armored, body.is_primary).await?;
    Ok((StatusCode::CREATED, Json(KeyResponse::from(stored))))
}

async fn generate_key(
    State(state): State<AuthState>,
    AuthUser(user_id): AuthUser,
    Json(body): Json<GenerateKeyRequest>,
) -> Result<(StatusCode, Json<KeyResponse>), OpengpgError> {
    let algo = KeyAlgorithm::parse(body.algorithm.as_deref().unwrap_or("rsa4096"))?;
    let email = body.email.clone();
    let name = body.name.clone();
    let passphrase = body.passphrase.clone();
    // RSA-4096 keygen is CPU-heavy; always off the async runtime.
    let parsed = tokio::task::spawn_blocking(move || {
        generate_keypair(&email, &name, &passphrase, algo)
    })
    .await
    .map_err(|e| OpengpgError::InvalidInput(format!("keygen task failed: {e}")))??;

    let stored = super::store::insert_key(&state.db, &user_id, &parsed, true).await?;
    Ok((StatusCode::CREATED, Json(KeyResponse::from(stored))))
}

async fn patch_key(
    State(state): State<AuthState>,
    AuthUser(user_id): AuthUser,
    Path(id): Path<String>,
    Json(body): Json<PatchKeyRequest>,
) -> Result<Json<KeyResponse>, OpengpgError> {
    if body.is_primary == Some(true) {
        let stored = set_primary(&state.db, &user_id, &id).await?;
        return Ok(Json(KeyResponse::from(stored)));
    }
    if body.is_primary == Some(false) {
        return Err(OpengpgError::InvalidInput(
            "clearing primary requires promoting another key".into(),
        ));
    }
    let key = get_key(&state.db, &user_id, &id)
        .await?
        .ok_or(OpengpgError::NotFound)?;
    Ok(Json(KeyResponse::from(key)))
}

async fn delete_key_handler(
    State(state): State<AuthState>,
    AuthUser(user_id): AuthUser,
    Path(id): Path<String>,
) -> Result<StatusCode, OpengpgError> {
    delete_key(&state.db, &user_id, &id).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn export_key(
    State(state): State<AuthState>,
    AuthUser(user_id): AuthUser,
    Path(id): Path<String>,
    Query(q): Query<ExportQuery>,
    headers: HeaderMap,
) -> Result<Json<ExportResponse>, axum::response::Response> {
    let key = get_key(&state.db, &user_id, &id)
        .await
        .map_err(IntoResponse::into_response)?
        .ok_or_else(|| OpengpgError::NotFound.into_response())?;

    if q.include_secret {
        if key.is_secret {
            let password = headers
                .get("x-lyra-current-password")
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| {
                    (
                        StatusCode::UNAUTHORIZED,
                        Json(serde_json::json!({
                            "error": "secret export requires X-Lyra-Current-Password"
                        })),
                    )
                        .into_response()
                })?;
            verify_current_password(&state, &user_id, password)
                .await
                .map_err(|e: AuthError| e.into_response())?;
            let armored = export_armored(&state.db, &user_id, &id)
                .await
                .map_err(IntoResponse::into_response)?;
            return Ok(Json(ExportResponse {
                armored,
                is_secret: true,
            }));
        }
        // Public-only row: includeSecret is a no-op.
        let armored = export_armored(&state.db, &user_id, &id)
            .await
            .map_err(IntoResponse::into_response)?;
        return Ok(Json(ExportResponse {
            armored,
            is_secret: false,
        }));
    }

    let armored = export_public_armored(&state.db, &user_id, &id)
        .await
        .map_err(IntoResponse::into_response)?;
    Ok(Json(ExportResponse {
        armored,
        is_secret: false,
    }))
}
