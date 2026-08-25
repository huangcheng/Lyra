//! OpenGPG keys HTTP API (`/api/v1/opengpg/keys`).

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
};
use serde::{Deserialize, Serialize};

use super::keys::{
    KeyAlgorithm, OpengpgError, generate_keypair, verify_secret_passphrase,
};
use super::session::{
    CacheMode, DEFAULT_TTL_MINUTES, MAX_TTL_MINUTES,
};
use super::store::{
    StoredKey, delete_key, export_armored, export_public_armored, get_key, import_armored,
    list_keys, set_primary,
};
use crate::auth::{AuthError, AuthSession, AuthState, AuthUser, verify_current_password};
use crate::kv::KvStore;
use zeroize::Zeroizing;

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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UnlockRequest {
    key_id: String,
    passphrase: String,
    /// `once` | `timed` | `session`
    cache: String,
    /// Timed TTL minutes (1–120); ignored for once/session.
    #[serde(default)]
    ttl_minutes: Option<u32>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct UnlockResponse {
    key_id: String,
    cache: String,
    /// True when the passphrase was accepted.
    unlocked: bool,
    /// True when the passphrase was retained in the session ring.
    cached: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LockRequest {
    /// When set, lock only this key; otherwise clear the whole session ring.
    #[serde(default)]
    key_id: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct LockResponse {
    unlocked_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct PassphraseCachePref {
    mode: String,
    #[serde(default = "default_ttl")]
    ttl_minutes: u32,
}

fn default_ttl() -> u32 {
    DEFAULT_TTL_MINUTES
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct OpengpgSettings {
    passphrase_cache: PassphraseCachePref,
}

impl Default for OpengpgSettings {
    fn default() -> Self {
        Self {
            passphrase_cache: PassphraseCachePref {
                mode: "timed".into(),
                ttl_minutes: DEFAULT_TTL_MINUTES,
            },
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PatchOpengpgSettings {
    passphrase_cache: Option<PassphraseCachePref>,
}

const UNLOCK_RL_MAX: i64 = 5;
const UNLOCK_RL_TTL: u64 = 15 * 60;

impl IntoResponse for OpengpgError {
    fn into_response(self) -> axum::response::Response {
        let (status, message) = match &self {
            OpengpgError::NotFound => (StatusCode::NOT_FOUND, self.to_string()),
            OpengpgError::InvalidKey(_) | OpengpgError::MissingEmail | OpengpgError::InvalidInput(_) => {
                (StatusCode::BAD_REQUEST, self.to_string())
            }
            OpengpgError::Conflict(_) => (StatusCode::CONFLICT, self.to_string()),
            OpengpgError::Unauthorized(_) => (StatusCode::UNAUTHORIZED, self.to_string()),
            OpengpgError::TooManyRequests => (StatusCode::TOO_MANY_REQUESTS, self.to_string()),
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
        .route("/api/v1/opengpg/unlock", post(unlock_key))
        .route("/api/v1/opengpg/lock", post(lock_keys))
        .route(
            "/api/v1/settings/opengpg",
            get(get_opengpg_settings).patch(patch_opengpg_settings),
        )
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

fn unlock_rl_key(token: &str) -> String {
    format!("rl:opengpg-unlock:{token}")
}

async fn ensure_unlock_allowed(kv: &dyn KvStore, token: &str) -> Result<(), OpengpgError> {
    let key = unlock_rl_key(token);
    let attempts = kv
        .get(&key)
        .await
        .map_err(|e| OpengpgError::InvalidInput(e.to_string()))?
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(0);
    if attempts >= UNLOCK_RL_MAX {
        return Err(OpengpgError::TooManyRequests);
    }
    Ok(())
}

async fn note_unlock_failure(kv: &dyn KvStore, token: &str) -> Result<(), OpengpgError> {
    let key = unlock_rl_key(token);
    let attempts = kv
        .get(&key)
        .await
        .map_err(|e| OpengpgError::InvalidInput(e.to_string()))?
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(0)
        + 1;
    kv.set(&key, &attempts.to_string(), Some(UNLOCK_RL_TTL))
        .await
        .map_err(|e| OpengpgError::InvalidInput(e.to_string()))?;
    Ok(())
}

async fn clear_unlock_failures(kv: &dyn KvStore, token: &str) {
    let _ = kv.del(&unlock_rl_key(token)).await;
}

async fn unlock_key(
    State(state): State<AuthState>,
    session: AuthSession,
    Json(body): Json<UnlockRequest>,
) -> Result<Json<UnlockResponse>, OpengpgError> {
    ensure_unlock_allowed(state.kv().as_ref(), &session.token).await?;

    let mode = CacheMode::parse(&body.cache).map_err(OpengpgError::InvalidInput)?;
    let ttl = body
        .ttl_minutes
        .unwrap_or(DEFAULT_TTL_MINUTES)
        .clamp(1, MAX_TTL_MINUTES);

    let key = get_key(&state.db, &session.user_id, &body.key_id)
        .await?
        .ok_or(OpengpgError::NotFound)?;
    if !key.is_secret {
        return Err(OpengpgError::InvalidInput(
            "cannot unlock a public-only key".into(),
        ));
    }

    let key_data = key.key_data.clone();
    let passphrase = body.passphrase.clone();
    let verify = tokio::task::spawn_blocking(move || {
        verify_secret_passphrase(&key_data, &passphrase)
    })
    .await
    .map_err(|e| OpengpgError::InvalidInput(format!("unlock task failed: {e}")))?;

    if let Err(e) = verify {
        note_unlock_failure(state.kv().as_ref(), &session.token).await?;
        // Map passphrase rejection to 401 without leaking crypto detail.
        return Err(match e {
            OpengpgError::InvalidInput(_) => {
                OpengpgError::Unauthorized("passphrase rejected".into())
            }
            other => other,
        });
    }
    clear_unlock_failures(state.kv().as_ref(), &session.token).await;

    state.opengpg_unlock.put(
        &session.token,
        &body.key_id,
        Zeroizing::new(body.passphrase),
        mode,
        ttl,
    );

    let cached = state
        .opengpg_unlock
        .is_unlocked(&session.token, &body.key_id);

    Ok(Json(UnlockResponse {
        key_id: body.key_id,
        cache: mode.as_str().into(),
        unlocked: true,
        cached,
    }))
}

async fn lock_keys(
    State(state): State<AuthState>,
    session: AuthSession,
    body: Option<Json<LockRequest>>,
) -> Result<Json<LockResponse>, OpengpgError> {
    let key_id = body.and_then(|Json(b)| b.key_id);
    state
        .opengpg_unlock
        .lock(&session.token, key_id.as_deref());
    Ok(Json(LockResponse {
        unlocked_ids: state.opengpg_unlock.unlocked_ids(&session.token),
    }))
}

fn settings_kv_key(user_id: &str) -> String {
    format!("user:{user_id}:opengpg")
}

async fn load_opengpg_settings(kv: &dyn KvStore, user_id: &str) -> OpengpgSettings {
    let Ok(Some(raw)) = kv.get(&settings_kv_key(user_id)).await else {
        return OpengpgSettings::default();
    };
    serde_json::from_str(&raw).unwrap_or_default()
}

async fn get_opengpg_settings(
    State(state): State<AuthState>,
    AuthUser(user_id): AuthUser,
) -> Result<Json<OpengpgSettings>, OpengpgError> {
    Ok(Json(
        load_opengpg_settings(state.kv().as_ref(), &user_id).await,
    ))
}

async fn patch_opengpg_settings(
    State(state): State<AuthState>,
    AuthUser(user_id): AuthUser,
    Json(body): Json<PatchOpengpgSettings>,
) -> Result<Json<OpengpgSettings>, OpengpgError> {
    let mut settings = load_opengpg_settings(state.kv().as_ref(), &user_id).await;
    if let Some(cache) = body.passphrase_cache {
        let _ = CacheMode::parse(&cache.mode).map_err(OpengpgError::InvalidInput)?;
        if !(1..=MAX_TTL_MINUTES).contains(&cache.ttl_minutes) {
            return Err(OpengpgError::InvalidInput(format!(
                "ttlMinutes must be 1–{MAX_TTL_MINUTES}"
            )));
        }
        settings.passphrase_cache = cache;
    }
    let raw = serde_json::to_string(&settings)
        .map_err(|e| OpengpgError::InvalidInput(e.to_string()))?;
    state
        .kv()
        .set(&settings_kv_key(&user_id), &raw, None)
        .await
        .map_err(|e| OpengpgError::InvalidInput(e.to_string()))?;
    Ok(Json(settings))
}
