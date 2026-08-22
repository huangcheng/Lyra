//! Authentication module.
//!
//! v1: Username/password + optional TOTP.
//! Single-user instance; the handler layer stays thin; real logic lives behind functions.

use argon2::{
    Argon2, PasswordHash, PasswordHasher, PasswordVerifier,
    password_hash::{SaltString, rand_core::OsRng},
};
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::IntoResponse,
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use totp_rs::{Algorithm, TOTP};
use uuid::Uuid;

use crate::crypto::{self, CryptoError};
use crate::kernel::App;
use crate::kv::KvStore;
#[cfg(test)]
use crate::kv::MemoryKv;
use crate::storage::DbPool;

// ── Public types ────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct LoginResponse {
    pub token: String,
    pub user: UserInfo,
    pub requires_totp: bool,
}

#[derive(Serialize)]
pub struct UserInfo {
    pub id: String,
    pub username: String,
    pub display_name: Option<String>,
    pub locale: String,
    pub totp_enabled: bool,
}

#[derive(Serialize)]
pub struct AuthStatus {
    pub has_user: bool,
    pub totp_enabled: bool,
}

#[derive(Serialize)]
pub struct TotpEnrollResponse {
    pub secret: String,
    pub otpauth_uri: String,
}

#[derive(Debug, Serialize)]
pub struct ErrorResponse {
    pub error: String,
}

// ── Request types ───────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct BootstrapRequest {
    pub username: String,
    pub password: String,
    pub display_name: Option<String>,
    pub locale: Option<String>,
}

#[derive(Deserialize)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
}

#[derive(Deserialize)]
pub struct TotpVerifyRequest {
    pub code: String,
    pub pending_token: String,
}

#[derive(Deserialize)]
pub struct TotpEnrollConfirmRequest {
    pub code: String,
}

// ── Password hashing ────────────────────────────────────────────────

fn hash_password(password: &str) -> Result<String, StatusCode> {
    let salt = SaltString::generate(&mut OsRng);
    let argon2 = Argon2::default();
    let hash = argon2
        .hash_password(password.as_bytes(), &salt)
        .map_err(|e| {
            tracing::error!("Password hashing failed: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .to_string();
    Ok(hash)
}

fn verify_password(password: &str, hash: &str) -> Result<bool, StatusCode> {
    let parsed_hash = PasswordHash::new(hash).map_err(|e| {
        tracing::error!("Invalid password hash format: {e}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    Ok(Argon2::default()
        .verify_password(password.as_bytes(), &parsed_hash)
        .is_ok())
}

fn validate_password(password: &str, min_length: usize) -> Result<(), String> {
    if password.len() < min_length {
        return Err(format!("Password must be at least {min_length} characters"));
    }
    if !password.chars().any(|c| c.is_ascii_uppercase()) {
        return Err("Password must contain at least one uppercase letter".to_string());
    }
    if !password.chars().any(|c| c.is_ascii_lowercase()) {
        return Err("Password must contain at least one lowercase letter".to_string());
    }
    if !password.chars().any(|c| c.is_ascii_digit()) {
        return Err("Password must contain at least one digit".to_string());
    }
    Ok(())
}

// ── Token management ────────────────────────────────────────────────

fn generate_token() -> String {
    use rand::RngCore;
    let mut rng = rand::thread_rng();
    let mut bytes = [0u8; 32];
    rng.fill_bytes(&mut bytes);
    base32::encode(base32::Alphabet::Rfc4648 { padding: false }, &bytes).to_lowercase()
}

// ── User data ───────────────────────────────────────────────────────

struct UserData {
    id: String,
    username: String,
    password_hash: Option<String>,
    totp_enabled: bool,
    totp_secret: Option<String>,
    display_name: Option<String>,
    locale: String,
}

// ── Database operations ─────────────────────────────────────────────

async fn has_any_user(db: &DbPool) -> Result<bool, StatusCode> {
    let count: i64 = match db {
        DbPool::Sqlite(pool) => {
            sqlx::query_scalar("SELECT COUNT(*) FROM lyra_user")
                .fetch_one(pool)
                .await
        }
        #[cfg(feature = "postgres")]
        DbPool::Postgres(pool) => {
            sqlx::query_scalar("SELECT COUNT(*) FROM lyra_user")
                .fetch_one(pool)
                .await
        }
    }
    .map_err(|e| {
        tracing::error!("DB error checking user count: {e}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    Ok(count > 0)
}

async fn find_first_user_totp_enabled(db: &DbPool) -> Result<bool, StatusCode> {
    match db {
        DbPool::Sqlite(pool) => {
            let row = sqlx::query("SELECT totp_enabled FROM lyra_user LIMIT 1")
                .fetch_optional(pool)
                .await
                .map_err(|e| {
                    tracing::error!("DB error: {e}");
                    StatusCode::INTERNAL_SERVER_ERROR
                })?;
            Ok(row.is_some_and(|r| r.get::<bool, _>("totp_enabled")))
        }
        #[cfg(feature = "postgres")]
        DbPool::Postgres(pool) => {
            let row = sqlx::query("SELECT totp_enabled FROM lyra_user LIMIT 1")
                .fetch_optional(pool)
                .await
                .map_err(|e| {
                    tracing::error!("DB error: {e}");
                    StatusCode::INTERNAL_SERVER_ERROR
                })?;
            Ok(row.is_some_and(|r| r.get::<bool, _>("totp_enabled")))
        }
    }
}

async fn insert_user(
    db: &DbPool,
    id: &str,
    username: &str,
    password_hash: &str,
    display_name: Option<&str>,
    locale: &str,
    encrypted_dek: &str,
) -> Result<(), StatusCode> {
    match db {
        DbPool::Sqlite(pool) => {
            sqlx::query(
                "INSERT INTO lyra_user (id, username, password_hash, display_name, locale, encrypted_dek) VALUES (?, ?, ?, ?, ?, ?)",
            )
            .bind(id)
            .bind(username)
            .bind(password_hash)
            .bind(display_name)
            .bind(locale)
            .bind(encrypted_dek)
            .execute(pool)
            .await
            .map_err(|e| {
                tracing::error!("Failed to insert user: {e}");
                StatusCode::INTERNAL_SERVER_ERROR
            })?;
        }
        #[cfg(feature = "postgres")]
        DbPool::Postgres(pool) => {
            sqlx::query(
                "INSERT INTO lyra_user (id, username, password_hash, display_name, locale, encrypted_dek) VALUES ($1, $2, $3, $4, $5, $6)",
            )
            .bind(id)
            .bind(username)
            .bind(password_hash)
            .bind(display_name)
            .bind(locale)
            .bind(encrypted_dek)
            .execute(pool)
            .await
            .map_err(|e| {
                tracing::error!("Failed to insert user: {e}");
                StatusCode::INTERNAL_SERVER_ERROR
            })?;
        }
    }
    Ok(())
}

async fn find_user_by_username(
    db: &DbPool,
    username: &str,
) -> Result<Option<UserData>, StatusCode> {
    match db {
        DbPool::Sqlite(pool) => {
            let row = sqlx::query(
                "SELECT id, username, password_hash, totp_enabled, totp_secret, display_name, locale FROM lyra_user WHERE username = ?",
            )
            .bind(username)
            .fetch_optional(pool)
            .await
            .map_err(|e| {
                tracing::error!("DB error finding user: {e}");
                StatusCode::INTERNAL_SERVER_ERROR
            })?;
            Ok(row.map(|r| UserData {
                id: r.get("id"),
                username: r.get("username"),
                password_hash: Some(r.get("password_hash")),
                totp_enabled: r.get("totp_enabled"),
                totp_secret: r.get("totp_secret"),
                display_name: r.get("display_name"),
                locale: r.get("locale"),
            }))
        }
        #[cfg(feature = "postgres")]
        DbPool::Postgres(pool) => {
            let row = sqlx::query(
                "SELECT id, username, password_hash, totp_enabled, totp_secret, display_name, locale FROM lyra_user WHERE username = $1",
            )
            .bind(username)
            .fetch_optional(pool)
            .await
            .map_err(|e| {
                tracing::error!("DB error finding user: {e}");
                StatusCode::INTERNAL_SERVER_ERROR
            })?;
            Ok(row.map(|r| UserData {
                id: r.get("id"),
                username: r.get("username"),
                password_hash: Some(r.get("password_hash")),
                totp_enabled: r.get("totp_enabled"),
                totp_secret: r.get("totp_secret"),
                display_name: r.get("display_name"),
                locale: r.get("locale"),
            }))
        }
    }
}

async fn find_user_by_id(db: &DbPool, user_id: &str) -> Result<Option<UserData>, StatusCode> {
    match db {
        DbPool::Sqlite(pool) => {
            let row = sqlx::query(
                "SELECT id, username, totp_enabled, totp_secret, display_name, locale FROM lyra_user WHERE id = ?",
            )
            .bind(user_id)
            .fetch_optional(pool)
            .await
            .map_err(|e| {
                tracing::error!("DB error finding user by ID: {e}");
                StatusCode::INTERNAL_SERVER_ERROR
            })?;
            Ok(row.map(|r| UserData {
                id: r.get("id"),
                username: r.get("username"),
                password_hash: None,
                totp_enabled: r.get("totp_enabled"),
                totp_secret: r.get("totp_secret"),
                display_name: r.get("display_name"),
                locale: r.get("locale"),
            }))
        }
        #[cfg(feature = "postgres")]
        DbPool::Postgres(pool) => {
            let row = sqlx::query(
                "SELECT id, username, totp_enabled, totp_secret, display_name, locale FROM lyra_user WHERE id = $1",
            )
            .bind(user_id)
            .fetch_optional(pool)
            .await
            .map_err(|e| {
                tracing::error!("DB error finding user by ID: {e}");
                StatusCode::INTERNAL_SERVER_ERROR
            })?;
            Ok(row.map(|r| UserData {
                id: r.get("id"),
                username: r.get("username"),
                password_hash: None,
                totp_enabled: r.get("totp_enabled"),
                totp_secret: r.get("totp_secret"),
                display_name: r.get("display_name"),
                locale: r.get("locale"),
            }))
        }
    }
}

async fn update_user_totp(
    db: &DbPool,
    user_id: &str,
    totp_secret: Option<&str>,
    totp_enabled: bool,
) -> Result<(), StatusCode> {
    match db {
        DbPool::Sqlite(pool) => {
            sqlx::query(
                "UPDATE lyra_user SET totp_secret = ?, totp_enabled = ?, updated_at = datetime('now') WHERE id = ?",
            )
            .bind(totp_secret)
            .bind(totp_enabled)
            .bind(user_id)
            .execute(pool)
            .await
            .map_err(|e| {
                tracing::error!("Failed to update TOTP: {e}");
                StatusCode::INTERNAL_SERVER_ERROR
            })?;
        }
        #[cfg(feature = "postgres")]
        DbPool::Postgres(pool) => {
            sqlx::query(
                "UPDATE lyra_user SET totp_secret = $1, totp_enabled = $2, updated_at = NOW() WHERE id = $3",
            )
            .bind(totp_secret)
            .bind(totp_enabled)
            .bind(user_id)
            .execute(pool)
            .await
            .map_err(|e| {
                tracing::error!("Failed to update TOTP: {e}");
                StatusCode::INTERNAL_SERVER_ERROR
            })?;
        }
    }
    Ok(())
}

// ── Session store (KvStore + sess_epoch) ─────────────────────────────

use std::sync::Arc;

fn sess_key(epoch: i64, token: &str) -> String {
    format!("sess:{epoch}:{token}")
}

fn tok_key(token: &str) -> String {
    format!("tok:{token}")
}

fn pending_key(token: &str) -> String {
    format!("pending:{token}")
}

async fn fetch_sess_epoch(db: &DbPool, user_id: &str) -> Result<i64, StatusCode> {
    let epoch: i64 = match db {
        DbPool::Sqlite(pool) => sqlx::query_scalar("SELECT sess_epoch FROM lyra_user WHERE id = ?")
            .bind(user_id)
            .fetch_optional(pool)
            .await
            .map_err(|e| {
                tracing::error!("fetch sess_epoch failed: {e}");
                StatusCode::INTERNAL_SERVER_ERROR
            })?
            .ok_or(StatusCode::NOT_FOUND)?,
        #[cfg(feature = "postgres")]
        DbPool::Postgres(pool) => {
            sqlx::query_scalar("SELECT sess_epoch FROM lyra_user WHERE id = $1")
                .bind(user_id)
                .fetch_optional(pool)
                .await
                .map_err(|e| {
                    tracing::error!("fetch sess_epoch failed: {e}");
                    StatusCode::INTERNAL_SERVER_ERROR
                })?
                .ok_or(StatusCode::NOT_FOUND)?
        }
    };
    Ok(epoch)
}

/// Bump `sess_epoch` and delete session keys for the previous epoch.
#[allow(dead_code)] // wired to password-change in a later task
pub async fn invalidate_user_sessions(
    pool: &DbPool,
    kv: &dyn KvStore,
    user_id: &str,
) -> Result<(), StatusCode> {
    let old_epoch = fetch_sess_epoch(pool, user_id).await?;
    match pool {
        DbPool::Sqlite(db) => {
            sqlx::query("UPDATE lyra_user SET sess_epoch = sess_epoch + 1 WHERE id = ?")
                .bind(user_id)
                .execute(db)
                .await
        }
        #[cfg(feature = "postgres")]
        DbPool::Postgres(db) => {
            sqlx::query("UPDATE lyra_user SET sess_epoch = sess_epoch + 1 WHERE id = $1")
                .bind(user_id)
                .execute(db)
                .await
        }
    }
    .map_err(|e| {
        tracing::error!("bump sess_epoch failed: {e}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    kv.del_prefix(&format!("sess:{old_epoch}:"))
        .await
        .map_err(|e| {
            tracing::error!("del_prefix sessions failed: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    Ok(())
}

#[derive(Clone)]
pub struct SessionStore {
    kv: Arc<dyn KvStore>,
    db: DbPool,
}

impl SessionStore {
    pub fn new(db: DbPool, kv: Arc<dyn KvStore>) -> Self {
        Self { kv, db }
    }

    #[must_use]
    #[allow(dead_code)]
    pub fn kv(&self) -> &Arc<dyn KvStore> {
        &self.kv
    }

    pub async fn create_session(&self, user_id: &str) -> Result<String, StatusCode> {
        let epoch = fetch_sess_epoch(&self.db, user_id).await?;
        let token = generate_token();
        self.kv
            .set(&sess_key(epoch, &token), user_id, None)
            .await
            .map_err(|e| {
                tracing::error!("session set failed: {e}");
                StatusCode::INTERNAL_SERVER_ERROR
            })?;
        // Reverse index so get_session can resolve token → user, then check current epoch.
        self.kv
            .set(&tok_key(&token), user_id, None)
            .await
            .map_err(|e| {
                tracing::error!("session tok index set failed: {e}");
                StatusCode::INTERNAL_SERVER_ERROR
            })?;
        Ok(token)
    }

    pub async fn create_pending_session(&self, user_id: &str) -> Result<String, StatusCode> {
        let token = generate_token();
        self.kv
            .set(&pending_key(&token), user_id, None)
            .await
            .map_err(|e| {
                tracing::error!("pending session set failed: {e}");
                StatusCode::INTERNAL_SERVER_ERROR
            })?;
        Ok(token)
    }

    pub async fn get_session(&self, token: &str) -> Option<String> {
        let user_id = self.kv.get(&tok_key(token)).await.ok().flatten()?;
        let epoch = fetch_sess_epoch(&self.db, &user_id).await.ok()?;
        let stored = self.kv.get(&sess_key(epoch, token)).await.ok().flatten()?;
        if stored == user_id {
            Some(user_id)
        } else {
            None
        }
    }

    pub async fn get_pending_session(&self, token: &str) -> Option<String> {
        self.kv.get(&pending_key(token)).await.ok().flatten()
    }

    pub async fn promote_pending_session(
        &self,
        pending_token: &str,
    ) -> Result<Option<String>, StatusCode> {
        let Some(user_id) = self.get_pending_session(pending_token).await else {
            return Ok(None);
        };
        self.kv
            .del(&pending_key(pending_token))
            .await
            .map_err(|e| {
                tracing::error!("pending session del failed: {e}");
                StatusCode::INTERNAL_SERVER_ERROR
            })?;
        Ok(Some(self.create_session(&user_id).await?))
    }

    pub async fn remove_session(&self, token: &str) {
        if let Some(user_id) = self.kv.get(&tok_key(token)).await.ok().flatten()
            && let Ok(epoch) = fetch_sess_epoch(&self.db, &user_id).await
        {
            let _ = self.kv.del(&sess_key(epoch, token)).await;
        }
        let _ = self.kv.del(&tok_key(token)).await;
    }
}

// ── Master key & DEK hierarchy ──────────────────────────────────────
//
// Master key (from `LYRA_MASTER_KEY`, validated at boot in config.rs)
//   → per-user KEK (HKDF-SHA256, info string bound to the user id)
//   → wraps a random 256-bit DEK stored in `lyra_user.encrypted_dek`
//   → DEK encrypts account credentials and the TOTP secret.
//
// See `docs/specs/2026-08-20-lyra-data-model-spec.md` §3.

/// Process-wide master key, installed once at boot by [`AuthState::new`].
static MASTER_KEY: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();

/// Install the master key. The first install wins; later calls are no-ops
/// (tests share one process-wide key).
pub(crate) fn install_master_key(key: &[u8]) {
    let _ = MASTER_KEY.set(key.to_vec());
}

fn master_key() -> Result<&'static [u8], CryptoError> {
    MASTER_KEY
        .get()
        .map(Vec::as_slice)
        .ok_or(CryptoError::MasterKeyNotInitialized)
}

/// Fixed master key shared by all tests in this crate (32+ bytes).
#[cfg(test)]
pub(crate) const TEST_MASTER_KEY: &[u8] = b"lyra-test-master-key-0123456789abcdef";

/// Install [`TEST_MASTER_KEY`] as the process-wide master key (idempotent).
#[cfg(test)]
pub(crate) fn install_test_master_key() {
    install_master_key(TEST_MASTER_KEY);
}

/// Fetch the wrapped DEK blob from `lyra_user.encrypted_dek`.
async fn fetch_encrypted_dek(db: &DbPool, user_id: &str) -> Result<String, CryptoError> {
    let wrapped: Option<String> = match db {
        DbPool::Sqlite(pool) => {
            sqlx::query_scalar("SELECT encrypted_dek FROM lyra_user WHERE id = ?")
                .bind(user_id)
                .fetch_optional(pool)
                .await
        }
        #[cfg(feature = "postgres")]
        DbPool::Postgres(pool) => {
            sqlx::query_scalar("SELECT encrypted_dek FROM lyra_user WHERE id = $1")
                .bind(user_id)
                .fetch_optional(pool)
                .await
        }
    }
    .map_err(|e| CryptoError::Storage(e.to_string()))?
    .flatten();
    wrapped.ok_or(CryptoError::MissingDek)
}

/// Map a crypto failure to a 500 response. The error text carries operator
/// guidance (never key material or plaintext).
fn crypto_err(e: &CryptoError) -> (StatusCode, Json<ErrorResponse>) {
    tracing::error!("crypto failure: {e}");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ErrorResponse {
            error: e.to_string(),
        }),
    )
}

// ── Application state ───────────────────────────────────────────────

#[derive(Clone)]
pub struct AuthState {
    pub db: DbPool,
    pub sessions: SessionStore,
    pub min_password_length: usize,
    pub data_dir: std::path::PathBuf,
    pub app: Arc<App>,
}

impl AuthState {
    pub fn new(
        db: DbPool,
        config: &crate::config::Config,
        app: Arc<App>,
        kv: Arc<dyn KvStore>,
    ) -> Result<Self, anyhow::Error> {
        std::fs::create_dir_all(&config.data_dir)?;
        install_master_key(&config.master_key);
        Ok(Self {
            db: db.clone(),
            sessions: SessionStore::new(db, kv),
            min_password_length: config.min_password_length,
            data_dir: std::path::PathBuf::from(&config.data_dir),
            app,
        })
    }

    /// Get the database pool.
    pub fn db(&self) -> &DbPool {
        &self.db
    }

    /// Get the user ID from the authenticated request.
    /// Extracts from request extensions set by the auth middleware.
    #[allow(dead_code)]
    #[allow(clippy::unused_self)]
    pub fn current_user_id(&self) -> Result<String, StatusCode> {
        // This is a placeholder - in practice, handlers should use
        // the AuthenticatedUser extension from the middleware
        Err(StatusCode::UNAUTHORIZED)
    }

    /// Get the user's data encryption key (DEK) for credential encryption.
    ///
    /// The DEK is a random 256-bit key generated at bootstrap, wrapped with
    /// the per-user KEK (HKDF-SHA256 from the master key, bound to the user
    /// id) and stored in `lyra_user.encrypted_dek`. This unwraps it on demand.
    ///
    /// # Errors
    /// Fails with a typed [`CryptoError`] if the master key was never
    /// installed, the user has no stored DEK, or unwrapping fails (e.g. the
    /// DEK was wrapped under a different master key — re-add accounts or
    /// reset the database).
    pub async fn get_user_dek(db: &DbPool, user_id: &str) -> Result<Vec<u8>, CryptoError> {
        let kek = crypto::derive_user_kek(master_key()?, user_id);
        let wrapped = fetch_encrypted_dek(db, user_id).await?;
        crypto::unwrap_dek(&kek, &wrapped)
    }
}

// ── Route handlers ──────────────────────────────────────────────────

/// Public auth routes (no middleware).
pub fn routes() -> Router<AuthState> {
    Router::new()
        .route("/api/auth/status", get(auth_status))
        .route("/api/auth/bootstrap", post(auth_bootstrap))
        .route("/api/auth/login", post(auth_login))
        .route("/api/auth/logout", post(auth_logout))
        .route("/api/auth/me", get(auth_me))
        .route("/api/auth/totp/enroll", post(totp_enroll))
        .route("/api/auth/totp/confirm", post(totp_enroll_confirm))
        .route("/api/auth/totp/verify", post(totp_verify))
        .route("/api/auth/totp/disable", post(totp_disable))
}

/// Middleware that enforces Bearer token authentication.
///
/// Validates the `Authorization: Bearer <token>` header against the session
/// store and injects the user id into request extensions for downstream
/// handlers.
#[allow(dead_code)]
pub async fn require_auth(
    State(state): State<AuthState>,
    headers: HeaderMap,
    mut req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Result<axum::response::Response, (StatusCode, Json<ErrorResponse>)> {
    let user_id = extract_session(&state, &headers).await?;
    req.extensions_mut().insert(AuthenticatedUser(user_id));
    Ok(next.run(req).await)
}

/// Extension type inserted by the auth middleware.
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct AuthenticatedUser(#[allow(dead_code)] pub String);

async fn auth_status(State(state): State<AuthState>) -> Json<AuthStatus> {
    let has_user = has_any_user(&state.db).await.unwrap_or(false);
    let totp_enabled = if has_user {
        find_first_user_totp_enabled(&state.db)
            .await
            .unwrap_or(false)
    } else {
        false
    };
    Json(AuthStatus {
        has_user,
        totp_enabled,
    })
}

async fn auth_bootstrap(
    State(state): State<AuthState>,
    Json(req): Json<BootstrapRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorResponse>)> {
    if has_any_user(&state.db).await.unwrap_or(false) {
        return Err((
            StatusCode::CONFLICT,
            Json(ErrorResponse {
                error: "A user already exists. Bootstrap is only available for first-time setup."
                    .to_string(),
            }),
        ));
    }

    if req.username.is_empty() || req.username.len() > 64 {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "Username must be between 1 and 64 characters".to_string(),
            }),
        ));
    }

    if let Err(msg) = validate_password(&req.password, state.min_password_length) {
        return Err((StatusCode::BAD_REQUEST, Json(ErrorResponse { error: msg })));
    }

    let password_hash = hash_password(&req.password).map_err(|e| {
        (
            e,
            Json(ErrorResponse {
                error: "Failed to hash password".to_string(),
            }),
        )
    })?;

    let user_id = Uuid::new_v7(uuid::Timestamp::now(uuid::NoContext)).to_string();
    let locale = req.locale.unwrap_or_else(|| "en".to_string());

    // Generate the user's DEK and store it wrapped with the per-user KEK.
    let dek = crypto::generate_key();
    let kek = crypto::derive_user_kek(master_key().map_err(|e| crypto_err(&e))?, &user_id);
    let wrapped_dek = crypto::wrap_dek(&kek, &dek).map_err(|e| crypto_err(&e))?;

    insert_user(
        &state.db,
        &user_id,
        &req.username,
        &password_hash,
        req.display_name.as_deref(),
        &locale,
        &wrapped_dek,
    )
    .await
    .map_err(|e| {
        (
            e,
            Json(ErrorResponse {
                error: "Failed to create user".to_string(),
            }),
        )
    })?;

    let token = state.sessions.create_session(&user_id).await.map_err(|e| {
        (
            e,
            Json(ErrorResponse {
                error: "Failed to create session".to_string(),
            }),
        )
    })?;

    Ok((
        StatusCode::CREATED,
        Json(LoginResponse {
            token,
            user: UserInfo {
                id: user_id,
                username: req.username,
                display_name: req.display_name,
                locale,
                totp_enabled: false,
            },
            requires_totp: false,
        }),
    ))
}

async fn auth_login(
    State(state): State<AuthState>,
    Json(req): Json<LoginRequest>,
) -> Result<Json<LoginResponse>, (StatusCode, Json<ErrorResponse>)> {
    let user = find_user_by_username(&state.db, &req.username)
        .await
        .map_err(|e| {
            (
                e,
                Json(ErrorResponse {
                    error: "Authentication failed".to_string(),
                }),
            )
        })?
        .ok_or_else(|| {
            (
                StatusCode::UNAUTHORIZED,
                Json(ErrorResponse {
                    error: "Invalid username or password".to_string(),
                }),
            )
        })?;

    let valid = verify_password(
        &req.password,
        &user.password_hash.ok_or_else(|| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: "Password hash not available".to_string(),
                }),
            )
        })?,
    )
    .map_err(|e| {
        (
            e,
            Json(ErrorResponse {
                error: "Authentication failed".to_string(),
            }),
        )
    })?;

    if !valid {
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(ErrorResponse {
                error: "Invalid username or password".to_string(),
            }),
        ));
    }

    if user.totp_enabled {
        let pending_token = state
            .sessions
            .create_pending_session(&user.id)
            .await
            .map_err(|e| {
                (
                    e,
                    Json(ErrorResponse {
                        error: "Failed to create pending session".to_string(),
                    }),
                )
            })?;
        return Ok(Json(LoginResponse {
            token: pending_token,
            user: UserInfo {
                id: user.id,
                username: user.username,
                display_name: user.display_name,
                locale: user.locale,
                totp_enabled: true,
            },
            requires_totp: true,
        }));
    }

    let token = state.sessions.create_session(&user.id).await.map_err(|e| {
        (
            e,
            Json(ErrorResponse {
                error: "Failed to create session".to_string(),
            }),
        )
    })?;

    Ok(Json(LoginResponse {
        token,
        user: UserInfo {
            id: user.id,
            username: user.username,
            display_name: user.display_name,
            locale: user.locale,
            totp_enabled: false,
        },
        requires_totp: false,
    }))
}

async fn totp_verify(
    State(state): State<AuthState>,
    Json(req): Json<TotpVerifyRequest>,
) -> Result<Json<LoginResponse>, (StatusCode, Json<ErrorResponse>)> {
    let user_id = state
        .sessions
        .get_pending_session(&req.pending_token)
        .await
        .ok_or_else(|| {
            (
                StatusCode::UNAUTHORIZED,
                Json(ErrorResponse {
                    error: "Invalid or expired pending session".to_string(),
                }),
            )
        })?;

    let user = find_user_by_id(&state.db, &user_id)
        .await
        .map_err(|e| {
            (
                e,
                Json(ErrorResponse {
                    error: "Failed to look up user".to_string(),
                }),
            )
        })?
        .ok_or_else(|| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: "User not found".to_string(),
                }),
            )
        })?;

    let stored_secret = user.totp_secret.as_deref().ok_or_else(|| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: "TOTP not configured".to_string(),
            }),
        )
    })?;
    let dek = AuthState::get_user_dek(&state.db, &user_id)
        .await
        .map_err(|e| crypto_err(&e))?;
    let secret = decrypt_totp_secret(&dek, stored_secret).map_err(|e| crypto_err(&e))?;
    let totp = build_totp(&secret, &user.username).map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: "Failed to build TOTP".to_string(),
            }),
        )
    })?;

    if !totp.check_current(&req.code).unwrap_or(false) {
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(ErrorResponse {
                error: "Invalid TOTP code".to_string(),
            }),
        ));
    }

    let token = state
        .sessions
        .promote_pending_session(&req.pending_token)
        .await
        .map_err(|e| {
            (
                e,
                Json(ErrorResponse {
                    error: "Failed to promote session".to_string(),
                }),
            )
        })?
        .ok_or_else(|| {
            (
                StatusCode::UNAUTHORIZED,
                Json(ErrorResponse {
                    error: "Invalid or expired pending session".to_string(),
                }),
            )
        })?;

    Ok(Json(LoginResponse {
        token,
        user: UserInfo {
            id: user_id,
            username: user.username,
            display_name: user.display_name,
            locale: user.locale,
            totp_enabled: true,
        },
        requires_totp: false,
    }))
}

async fn totp_enroll(
    State(state): State<AuthState>,
    headers: HeaderMap,
) -> Result<Json<TotpEnrollResponse>, (StatusCode, Json<ErrorResponse>)> {
    let user_id = extract_session(&state, &headers).await?;

    let user = find_user_by_id(&state.db, &user_id)
        .await
        .map_err(|e| {
            (
                e,
                Json(ErrorResponse {
                    error: "Failed to look up user".to_string(),
                }),
            )
        })?
        .ok_or_else(|| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: "User not found".to_string(),
                }),
            )
        })?;

    let secret_bytes = totp_rs::Secret::generate_secret().to_bytes().map_err(|e| {
        tracing::error!("Failed to generate TOTP secret: {e}");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: "Failed to generate TOTP secret".to_string(),
            }),
        )
    })?;

    let secret_base32 = base32::encode(base32::Alphabet::Rfc4648 { padding: false }, &secret_bytes);
    // Store the secret encrypted with the user's DEK; enabled only after confirm.
    let dek = AuthState::get_user_dek(&state.db, &user_id)
        .await
        .map_err(|e| crypto_err(&e))?;
    let stored_secret = encrypt_totp_secret(&dek, &secret_base32).map_err(|e| crypto_err(&e))?;
    update_user_totp(&state.db, &user_id, Some(&stored_secret), false)
        .await
        .map_err(|e| {
            (
                e,
                Json(ErrorResponse {
                    error: "Failed to store TOTP secret".to_string(),
                }),
            )
        })?;

    let totp = build_totp_from_raw(&secret_bytes, &user.username).map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: "Failed to build TOTP".to_string(),
            }),
        )
    })?;

    Ok(Json(TotpEnrollResponse {
        secret: secret_base32,
        otpauth_uri: totp.get_url(),
    }))
}

async fn totp_enroll_confirm(
    State(state): State<AuthState>,
    headers: HeaderMap,
    Json(req): Json<TotpEnrollConfirmRequest>,
) -> Result<Json<AuthStatus>, (StatusCode, Json<ErrorResponse>)> {
    let user_id = extract_session(&state, &headers).await?;

    let user = find_user_by_id(&state.db, &user_id)
        .await
        .map_err(|e| {
            (
                e,
                Json(ErrorResponse {
                    error: "Failed to look up user".to_string(),
                }),
            )
        })?
        .ok_or_else(|| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: "User not found".to_string(),
                }),
            )
        })?;

    let stored = user.totp_secret.ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "TOTP enrollment not started".to_string(),
            }),
        )
    })?;

    let dek = AuthState::get_user_dek(&state.db, &user_id)
        .await
        .map_err(|e| crypto_err(&e))?;
    let secret = decrypt_totp_secret(&dek, &stored).map_err(|e| crypto_err(&e))?;

    let totp = build_totp(&secret, &user.username).map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: "Failed to build TOTP".to_string(),
            }),
        )
    })?;

    if !totp.check_current(&req.code).unwrap_or(false) {
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(ErrorResponse {
                error: "Invalid TOTP code. Please try again.".to_string(),
            }),
        ));
    }

    // Code verified: keep the same encrypted secret, flip the enabled flag.
    update_user_totp(&state.db, &user_id, Some(&stored), true)
        .await
        .map_err(|e| {
            (
                e,
                Json(ErrorResponse {
                    error: "Failed to enable TOTP".to_string(),
                }),
            )
        })?;

    Ok(Json(AuthStatus {
        has_user: true,
        totp_enabled: true,
    }))
}

async fn totp_disable(
    State(state): State<AuthState>,
    headers: HeaderMap,
) -> Result<Json<AuthStatus>, (StatusCode, Json<ErrorResponse>)> {
    let user_id = extract_session(&state, &headers).await?;

    update_user_totp(&state.db, &user_id, None, false)
        .await
        .map_err(|e| {
            (
                e,
                Json(ErrorResponse {
                    error: "Failed to disable TOTP".to_string(),
                }),
            )
        })?;

    Ok(Json(AuthStatus {
        has_user: true,
        totp_enabled: false,
    }))
}

async fn auth_logout(
    State(state): State<AuthState>,
    headers: HeaderMap,
) -> Result<StatusCode, (StatusCode, Json<ErrorResponse>)> {
    if let Some(token) = extract_token_from_headers(&headers) {
        state.sessions.remove_session(&token).await;
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn auth_me(
    State(state): State<AuthState>,
    headers: HeaderMap,
) -> Result<Json<UserInfo>, (StatusCode, Json<ErrorResponse>)> {
    let user_id = extract_session(&state, &headers).await?;

    let user = find_user_by_id(&state.db, &user_id)
        .await
        .map_err(|e| {
            (
                e,
                Json(ErrorResponse {
                    error: "Failed to look up user".to_string(),
                }),
            )
        })?
        .ok_or_else(|| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: "User not found".to_string(),
                }),
            )
        })?;

    Ok(Json(UserInfo {
        id: user.id,
        username: user.username,
        display_name: user.display_name,
        locale: user.locale,
        totp_enabled: user.totp_enabled,
    }))
}

// ── Helpers ─────────────────────────────────────────────────────────

fn extract_token_from_headers(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(String::from)
}

async fn extract_session(
    state: &AuthState,
    headers: &HeaderMap,
) -> Result<String, (StatusCode, Json<ErrorResponse>)> {
    let token = extract_token_from_headers(headers).ok_or_else(|| {
        (
            StatusCode::UNAUTHORIZED,
            Json(ErrorResponse {
                error: "Missing authorization header".to_string(),
            }),
        )
    })?;

    state.sessions.get_session(&token).await.ok_or_else(|| {
        (
            StatusCode::UNAUTHORIZED,
            Json(ErrorResponse {
                error: "Invalid or expired session".to_string(),
            }),
        )
    })
}

/// Encrypt a base32 TOTP secret with the user's DEK; returns the JSON blob
/// stored in `lyra_user.totp_secret`.
fn encrypt_totp_secret(dek: &[u8], secret_base32: &str) -> Result<String, CryptoError> {
    let encrypted = crypto::encrypt(dek, secret_base32.as_bytes())?;
    serde_json::to_string(&encrypted).map_err(|e| CryptoError::Encrypt(e.to_string()))
}

/// Decrypt a stored TOTP secret blob back to its base32 form.
fn decrypt_totp_secret(dek: &[u8], stored: &str) -> Result<String, CryptoError> {
    let encrypted: crypto::EncryptedCredential = serde_json::from_str(stored).map_err(|e| {
        CryptoError::Decrypt(format!(
            "stored TOTP secret is not an encrypted blob ({e}); disable 2FA and re-enroll, or reset the database"
        ))
    })?;
    let bytes = crypto::decrypt(dek, &encrypted)?;
    String::from_utf8(bytes)
        .map_err(|e| CryptoError::Decrypt(format!("TOTP secret is not valid UTF-8: {e}")))
}

fn build_totp(secret_base32: &str, username: &str) -> Result<TOTP, StatusCode> {
    let secret_bytes = base32::decode(base32::Alphabet::Rfc4648 { padding: false }, secret_base32)
        .ok_or_else(|| {
            tracing::error!("Invalid TOTP secret encoding");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    build_totp_from_raw(&secret_bytes, username)
}

fn build_totp_from_raw(secret_bytes: &[u8], username: &str) -> Result<TOTP, StatusCode> {
    TOTP::new(
        Algorithm::SHA1,
        6,
        1,
        30,
        secret_bytes.to_vec(),
        Some("Lyra".to_string()),
        username.to_string(),
    )
    .map_err(|e| {
        tracing::error!("Failed to create TOTP: {e}");
        StatusCode::INTERNAL_SERVER_ERROR
    })
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_validation() {
        assert!(validate_password("Ab1", 8).is_err());
        assert!(validate_password("abcdefgh1", 8).is_err());
        assert!(validate_password("ABCDEFGH1", 8).is_err());
        assert!(validate_password("Abcdefgh", 8).is_err());
        assert!(validate_password("Abcdefg1", 8).is_ok());
        assert!(validate_password("Abcdefg1!@#", 8).is_ok());
    }

    #[test]
    fn password_hashing_roundtrip() {
        let password = "TestPassw0rd!";
        let hash = hash_password(password).unwrap();
        assert!(verify_password(password, &hash).unwrap());
        assert!(!verify_password("WrongPassw0rd!", &hash).unwrap());
    }

    #[test]
    fn token_generation() {
        let token1 = generate_token();
        let token2 = generate_token();
        assert_ne!(token1, token2);
        assert!(token1.len() > 20);
    }

    #[test]
    fn totp_roundtrip() {
        let secret_bytes = totp_rs::Secret::generate_secret().to_bytes().unwrap();
        let secret_base32 =
            base32::encode(base32::Alphabet::Rfc4648 { padding: false }, &secret_bytes);
        let totp = build_totp(&secret_base32, "testuser").unwrap();
        let code = totp.generate_current().unwrap();
        assert!(totp.check_current(&code).unwrap());
    }

    #[tokio::test]
    async fn session_store_operations() {
        let db = test_pool().await;
        seed_user(&db, "user-1").await;
        let store = SessionStore::new(db, Arc::new(MemoryKv::new()));
        let token = store.create_session("user-1").await.unwrap();
        assert_eq!(store.get_session(&token).await, Some("user-1".to_string()));
        store.remove_session(&token).await;
        assert_eq!(store.get_session(&token).await, None);
    }

    #[tokio::test]
    async fn pending_session_promotion() {
        let db = test_pool().await;
        seed_user(&db, "user-1").await;
        let store = SessionStore::new(db, Arc::new(MemoryKv::new()));
        let pending_token = store.create_pending_session("user-1").await.unwrap();
        assert!(store.get_pending_session(&pending_token).await.is_some());
        let session_token = store
            .promote_pending_session(&pending_token)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            store.get_session(&session_token).await,
            Some("user-1".to_string())
        );
        assert!(store.get_pending_session(&pending_token).await.is_none());
    }

    #[tokio::test]
    async fn bump_epoch_invalidates_old_tokens() {
        let db = test_pool().await;
        seed_user(&db, "user-1").await;
        let kv = Arc::new(MemoryKv::new());
        let store = SessionStore::new(db.clone(), Arc::clone(&kv) as Arc<dyn KvStore>);
        let token = store.create_session("user-1").await.unwrap();
        assert_eq!(store.get_session(&token).await, Some("user-1".to_string()));

        invalidate_user_sessions(&db, kv.as_ref(), "user-1")
            .await
            .unwrap();

        assert_eq!(store.get_session(&token).await, None);
        // New sessions at epoch 1 still work.
        let token2 = store.create_session("user-1").await.unwrap();
        assert_eq!(store.get_session(&token2).await, Some("user-1".to_string()));
    }

    async fn test_pool() -> DbPool {
        let storage = crate::storage::Storage::new("sqlite::memory:")
            .await
            .unwrap();
        storage.run_migrations().await.unwrap();
        storage.pool().clone()
    }

    async fn seed_user(db: &DbPool, id: &str) {
        match db {
            DbPool::Sqlite(pool) => {
                sqlx::query("INSERT INTO lyra_user (id, username, password_hash) VALUES (?, ?, ?)")
                    .bind(id)
                    .bind(format!("user-{id}"))
                    .bind("hash")
                    .execute(pool)
                    .await
                    .unwrap();
            }
            #[cfg(feature = "postgres")]
            DbPool::Postgres(_) => panic!("expected sqlite in tests"),
        }
    }

    // ── DEK hierarchy & TOTP-at-rest tests ──────────────────────────

    fn test_config() -> crate::config::Config {
        crate::config::Config {
            listen_addr: "127.0.0.1:0".into(),
            database_url: "sqlite::memory:".into(),
            data_dir: std::env::temp_dir().to_string_lossy().into_owned(),
            session_secret: b"test-session-secret-0123456789abcdef".to_vec(),
            min_password_length: 8,
            sync_max_concurrent: 3,
            sync_poll_secs: 300,
            redis_url: None,
            master_key: TEST_MASTER_KEY.to_vec(),
        }
    }

    fn test_state(db: DbPool) -> AuthState {
        AuthState::new(
            db,
            &test_config(),
            Arc::new(crate::kernel::App::new()),
            Arc::new(MemoryKv::new()),
        )
        .unwrap()
    }

    fn sqlite_pool(db: &DbPool) -> &sqlx::SqlitePool {
        match db {
            DbPool::Sqlite(pool) => pool,
            #[cfg(feature = "postgres")]
            DbPool::Postgres(_) => panic!("expected sqlite in tests"),
        }
    }

    /// Seed a user with a freshly generated DEK, stored wrapped.
    /// Returns the plaintext DEK for assertions.
    async fn seed_user_with_dek(db: &DbPool, id: &str) -> Vec<u8> {
        install_test_master_key();
        seed_user(db, id).await;
        let dek = crypto::generate_key();
        let kek = crypto::derive_user_kek(TEST_MASTER_KEY, id);
        let wrapped = crypto::wrap_dek(&kek, &dek).unwrap();
        sqlx::query("UPDATE lyra_user SET encrypted_dek = ? WHERE id = ?")
            .bind(&wrapped)
            .bind(id)
            .execute(sqlite_pool(db))
            .await
            .unwrap();
        dek.to_vec()
    }

    fn bearer_headers(token: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            format!("Bearer {token}").parse().unwrap(),
        );
        headers
    }

    #[tokio::test]
    async fn bootstrap_creates_and_persists_encrypted_dek() {
        let db = test_pool().await;
        let state = test_state(db.clone());

        auth_bootstrap(
            State(state),
            Json(BootstrapRequest {
                username: "alice".into(),
                password: "Str0ngPass1".into(),
                display_name: None,
                locale: None,
            }),
        )
        .await
        .unwrap();

        let pool = sqlite_pool(&db);
        let (user_id, stored): (String, String) =
            sqlx::query_as("SELECT id, encrypted_dek FROM lyra_user")
                .fetch_one(pool)
                .await
                .unwrap();

        // At rest it is a wrapped-key blob (JSON ciphertext + nonce), not raw key material.
        let blob: crypto::EncryptedCredential = serde_json::from_str(&stored).unwrap();
        assert!(!blob.ciphertext.is_empty());
        assert!(!blob.nonce.is_empty());

        // Unwrap round-trip via the public lookup matches a manual unwrap.
        let dek = AuthState::get_user_dek(&db, &user_id).await.unwrap();
        assert_eq!(dek.len(), 32);
        let kek = crypto::derive_user_kek(TEST_MASTER_KEY, &user_id);
        assert_eq!(crypto::unwrap_dek(&kek, &stored).unwrap(), dek);
    }

    #[tokio::test]
    async fn two_users_get_different_deks() {
        let db = test_pool().await;
        let dek_a = seed_user_with_dek(&db, "user-a").await;
        let dek_b = seed_user_with_dek(&db, "user-b").await;

        assert_eq!(AuthState::get_user_dek(&db, "user-a").await.unwrap(), dek_a);
        assert_eq!(AuthState::get_user_dek(&db, "user-b").await.unwrap(), dek_b);
        assert_ne!(dek_a, dek_b);

        // A user's wrapped DEK cannot be unwrapped with another user's KEK.
        let stored_a: String =
            sqlx::query_scalar("SELECT encrypted_dek FROM lyra_user WHERE id = 'user-a'")
                .fetch_one(sqlite_pool(&db))
                .await
                .unwrap();
        let kek_b = crypto::derive_user_kek(TEST_MASTER_KEY, "user-b");
        assert!(crypto::unwrap_dek(&kek_b, &stored_a).is_err());
    }

    #[tokio::test]
    async fn missing_dek_is_a_typed_error() {
        let db = test_pool().await;
        install_test_master_key();
        seed_user(&db, "user-legacy").await;

        let err = AuthState::get_user_dek(&db, "user-legacy").await.unwrap_err();
        assert!(matches!(err, CryptoError::MissingDek));
        assert!(err.to_string().contains("reset the database"));
    }

    #[tokio::test]
    async fn totp_secret_is_encrypted_at_rest() {
        let db = test_pool().await;
        let state = test_state(db.clone());

        auth_bootstrap(
            State(state.clone()),
            Json(BootstrapRequest {
                username: "alice".into(),
                password: "Str0ngPass1".into(),
                display_name: None,
                locale: None,
            }),
        )
        .await
        .unwrap();

        let user_id: String = sqlx::query_scalar("SELECT id FROM lyra_user")
            .fetch_one(sqlite_pool(&db))
            .await
            .unwrap();
        let token = state.sessions.create_session(&user_id).await.unwrap();

        // Enroll: returns the plaintext secret once, stores only ciphertext.
        let enroll = totp_enroll(State(state.clone()), bearer_headers(&token))
            .await
            .unwrap();
        let plaintext_secret = enroll.secret.clone();

        let stored: String =
            sqlx::query_scalar("SELECT totp_secret FROM lyra_user WHERE id = ?")
                .bind(&user_id)
                .fetch_one(sqlite_pool(&db))
                .await
                .unwrap();
        assert_ne!(stored, plaintext_secret);
        assert!(!stored.contains(&plaintext_secret));
        serde_json::from_str::<crypto::EncryptedCredential>(&stored).unwrap();
        let enabled: bool =
            sqlx::query_scalar("SELECT totp_enabled FROM lyra_user WHERE id = ?")
                .bind(&user_id)
                .fetch_one(sqlite_pool(&db))
                .await
                .unwrap();
        assert!(!enabled, "enroll alone must not enable 2FA");

        // Confirm with a valid code flips totp_enabled on.
        let totp = build_totp(&plaintext_secret, "alice").unwrap();
        let code = totp.generate_current().unwrap();
        let status = totp_enroll_confirm(
            State(state.clone()),
            bearer_headers(&token),
            Json(TotpEnrollConfirmRequest { code }),
        )
        .await
        .unwrap();
        assert!(status.totp_enabled);

        // The stored value still decrypts to the original secret.
        let dek = AuthState::get_user_dek(&db, &user_id).await.unwrap();
        let stored_after: String =
            sqlx::query_scalar("SELECT totp_secret FROM lyra_user WHERE id = ?")
                .bind(&user_id)
                .fetch_one(sqlite_pool(&db))
                .await
                .unwrap();
        assert_eq!(
            decrypt_totp_secret(&dek, &stored_after).unwrap(),
            plaintext_secret
        );
    }

    #[tokio::test]
    async fn legacy_plaintext_totp_secret_fails_loudly() {
        let db = test_pool().await;
        seed_user_with_dek(&db, "user-1").await;
        // Simulate a pre-fix row: plaintext base32 secret in the column.
        sqlx::query("UPDATE lyra_user SET totp_secret = 'JBSWY3DPEHPK3PXP' WHERE id = 'user-1'")
            .execute(sqlite_pool(&db))
            .await
            .unwrap();

        let dek = AuthState::get_user_dek(&db, "user-1").await.unwrap();
        let err = decrypt_totp_secret(&dek, "JBSWY3DPEHPK3PXP").unwrap_err();
        assert!(matches!(err, CryptoError::Decrypt(_)));
        assert!(err.to_string().contains("re-enroll"));
    }
}
