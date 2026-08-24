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
    extract::{FromRequestParts, State},
    http::{HeaderMap, StatusCode, header, request::Parts},
    response::IntoResponse,
    routing::{get, patch, post},
};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use totp_rs::{Algorithm, TOTP};
use uuid::Uuid;

use crate::crypto::{self, CryptoError};
use crate::db_row::{id_from_row, id_param};
use crate::kernel::App;
use crate::kv::KvStore;
#[cfg(test)]
use crate::kv::MemoryKv;
use crate::storage::DbPool;
use zeroize::Zeroizing;

// ── Public types ────────────────────────────────────────────────────

/// Sessions live for 7 days, fixed from creation. The kv seam has no
/// touch/expire operation, so rolling renewal would cost a write on every
/// authenticated request; a fixed TTL keeps session reads cheap.
const SESSION_TTL_SECS: u64 = 7 * 24 * 60 * 60;
/// Pending TOTP tokens (password ok, second factor outstanding) live 5 minutes.
const PENDING_TTL_SECS: u64 = 5 * 60;
/// Rate limit: 5 failed attempts within a 15-minute fixed window.
const RATE_LIMIT_MAX_ATTEMPTS: i64 = 5;
const RATE_LIMIT_WINDOW_SECS: u64 = 15 * 60;
/// The last accepted TOTP timestep is remembered for 2 minutes so a code
/// cannot be replayed inside its ±1-step validity window.
const TOTP_LAST_STEP_TTL_SECS: u64 = 120;
/// Password length cap: Argon2 is memory-hard, so unbounded inputs are a DoS.
const MAX_PASSWORD_LENGTH: usize = 1024;
/// User-facing message when bootstrap finds (or races with) an existing user.
const BOOTSTRAP_TAKEN: &str =
    "A user already exists. Bootstrap is only available for first-time setup.";

#[derive(Debug, Serialize)]
pub struct LoginResponse {
    pub token: String,
    pub user: UserInfo,
    pub requires_totp: bool,
}

#[derive(Debug, Serialize)]
pub struct UserInfo {
    pub id: String,
    pub username: String,
    pub display_name: Option<String>,
    pub locale: String,
    pub totp_enabled: bool,
    pub mark_read_policy: String,
}

#[derive(Debug, Deserialize)]
pub struct PreferencesRequest {
    #[serde(rename = "markReadPolicy")]
    pub mark_read_policy: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct AuthStatus {
    pub has_user: bool,
    pub totp_enabled: bool,
}

#[derive(Debug, Serialize)]
pub struct TotpEnrollResponse {
    pub secret: String,
    pub otpauth_uri: String,
}

#[derive(Debug, Serialize)]
pub struct ErrorResponse {
    pub error: String,
}

/// Typed error for auth endpoints.
///
/// `IntoResponse` preserves the exact status codes and user-facing messages
/// the handlers produced before; internal details are logged server-side and
/// only the fixed message strings reach the client.
#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("{0}")]
    BadRequest(String),
    #[error("{0}")]
    Unauthorized(String),
    #[error("Too many failed attempts. Try again later.")]
    TooManyRequests,
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    Internal(String),
    /// Crypto failures carry deliberate operator guidance (never key material).
    #[error("{0}")]
    Crypto(CryptoError),
}

impl AuthError {
    fn internal(op: &str) -> Self {
        Self::Internal(op.to_string())
    }

    fn unauthorized(msg: &str) -> Self {
        Self::Unauthorized(msg.to_string())
    }

    fn invalid_credentials() -> Self {
        Self::Unauthorized("Invalid username or password".to_string())
    }

    fn status(&self) -> StatusCode {
        match self {
            AuthError::BadRequest(_) => StatusCode::BAD_REQUEST,
            AuthError::Unauthorized(_) => StatusCode::UNAUTHORIZED,
            AuthError::TooManyRequests => StatusCode::TOO_MANY_REQUESTS,
            AuthError::Conflict(_) => StatusCode::CONFLICT,
            AuthError::Internal(_) | AuthError::Crypto(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl IntoResponse for AuthError {
    fn into_response(self) -> axum::response::Response {
        let status = self.status();
        (
            status,
            Json(ErrorResponse {
                error: self.to_string(),
            }),
        )
            .into_response()
    }
}

/// Authenticated user id, resolved from the `Authorization: Bearer` token
/// against the session store. The one shared auth check — handlers take this
/// as an extractor instead of parsing headers themselves.
pub struct AuthUser(pub String);

impl FromRequestParts<AuthState> for AuthUser {
    type Rejection = AuthError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AuthState,
    ) -> Result<Self, Self::Rejection> {
        extract_session(state, &parts.headers).await.map(Self)
    }
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

#[derive(Deserialize)]
pub struct TotpDisableRequest {
    pub password: String,
}

#[derive(Deserialize)]
pub struct ChangePasswordRequest {
    pub current_password: String,
    pub new_password: String,
}

// ── Password hashing ────────────────────────────────────────────────

async fn hash_password(password: &str) -> Result<String, AuthError> {
    let password = Zeroizing::new(password.to_owned());
    tokio::task::spawn_blocking(move || {
        let salt = SaltString::generate(&mut OsRng);
        let argon2 = Argon2::default();
        argon2
            .hash_password(password.as_bytes(), &salt)
            .map_err(|e| {
                tracing::error!("Password hashing failed: {e}");
                AuthError::internal("Failed to hash password")
            })
            .map(|h| h.to_string())
    })
    .await
    .map_err(|e| {
        tracing::error!("Password hash task failed: {e}");
        AuthError::internal("Failed to hash password")
    })?
}

async fn verify_password(password: &str, hash: &str) -> Result<bool, StatusCode> {
    let password = Zeroizing::new(password.to_owned());
    let hash = hash.to_owned();
    tokio::task::spawn_blocking(move || {
        let parsed_hash = PasswordHash::new(&hash).map_err(|e| {
            tracing::error!("Invalid password hash format: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
        Ok(Argon2::default()
            .verify_password(password.as_bytes(), &parsed_hash)
            .is_ok())
    })
    .await
    .map_err(|e| {
        tracing::error!("Password verify task failed: {e}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?
}

fn validate_password(password: &str, min_length: usize) -> Result<(), String> {
    if password.len() > MAX_PASSWORD_LENGTH {
        return Err(format!(
            "Password must be at most {MAX_PASSWORD_LENGTH} characters"
        ));
    }
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
    mark_read_policy: String,
}

fn parse_mark_read_policy(raw: &str) -> Result<String, AuthError> {
    match raw {
        "on_open" | "on_scroll_end" | "manual" => Ok(raw.to_string()),
        _ => Err(AuthError::BadRequest(
            "markReadPolicy must be on_open, on_scroll_end, or manual".into(),
        )),
    }
}

fn parse_stored_mark_read_policy(raw: String) -> String {
    if matches!(raw.as_str(), "on_open" | "on_scroll_end" | "manual") {
        raw
    } else {
        "on_open".to_string()
    }
}

fn user_info_from(user: &UserData) -> UserInfo {
    UserInfo {
        id: user.id.clone(),
        username: user.username.clone(),
        display_name: user.display_name.clone(),
        locale: user.locale.clone(),
        totp_enabled: user.totp_enabled,
        mark_read_policy: user.mark_read_policy.clone(),
    }
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
) -> Result<(), sqlx::Error> {
    let id = id_param(db, id)?;
    match db {
        DbPool::Sqlite(pool) => {
            sqlx::query(
                "INSERT INTO lyra_user (id, username, password_hash, display_name, locale, encrypted_dek) VALUES (?, ?, ?, ?, ?, ?)",
            )
            .bind(&id)
            .bind(username)
            .bind(password_hash)
            .bind(display_name)
            .bind(locale)
            .bind(encrypted_dek)
            .execute(pool)
            .await?;
        }
        #[cfg(feature = "postgres")]
        DbPool::Postgres(pool) => {
            sqlx::query(
                "INSERT INTO lyra_user (id, username, password_hash, display_name, locale, encrypted_dek) VALUES ($1, $2, $3, $4, $5, $6)",
            )
            .bind(&id)
            .bind(username)
            .bind(password_hash)
            .bind(display_name)
            .bind(locale)
            .bind(encrypted_dek)
            .execute(pool)
            .await?;
        }
    }
    Ok(())
}

/// True when `e` is a unique-constraint violation (e.g. the `singleton`
/// guard rejecting a second row, or a duplicate username).
fn is_unique_violation(e: &sqlx::Error) -> bool {
    match e {
        sqlx::Error::Database(db_err) => db_err.is_unique_violation(),
        _ => false,
    }
}

async fn find_user_by_username(db: &DbPool, username: &str) -> Result<Option<UserData>, AuthError> {
    match db {
        DbPool::Sqlite(pool) => {
            let row = sqlx::query(
                "SELECT id, username, password_hash, totp_enabled, totp_secret, display_name, locale, mark_read_policy FROM lyra_user WHERE username = ?",
            )
            .bind(username)
            .fetch_optional(pool)
            .await
            .map_err(|e| {
                tracing::error!("DB error finding user: {e}");
                AuthError::internal("Authentication failed")
            })?;
            Ok(row.map(|r| UserData {
                id: id_from_row(&r, "id"),
                username: r.get("username"),
                password_hash: Some(r.get("password_hash")),
                totp_enabled: r.get("totp_enabled"),
                totp_secret: r.get("totp_secret"),
                display_name: r.get("display_name"),
                locale: r.get("locale"),
                mark_read_policy: parse_stored_mark_read_policy(r.get("mark_read_policy")),
            }))
        }
        #[cfg(feature = "postgres")]
        DbPool::Postgres(pool) => {
            let row = sqlx::query(
                "SELECT id, username, password_hash, totp_enabled, totp_secret, display_name, locale, mark_read_policy FROM lyra_user WHERE username = $1",
            )
            .bind(username)
            .fetch_optional(pool)
            .await
            .map_err(|e| {
                tracing::error!("DB error finding user: {e}");
                AuthError::internal("Authentication failed")
            })?;
            Ok(row.map(|r| UserData {
                id: id_from_row(&r, "id"),
                username: r.get("username"),
                password_hash: Some(r.get("password_hash")),
                totp_enabled: r.get("totp_enabled"),
                totp_secret: r.get("totp_secret"),
                display_name: r.get("display_name"),
                locale: r.get("locale"),
                mark_read_policy: parse_stored_mark_read_policy(r.get("mark_read_policy")),
            }))
        }
    }
}

async fn find_user_by_id(db: &DbPool, user_id: &str) -> Result<Option<UserData>, AuthError> {
    let Ok(id) = id_param(db, user_id) else {
        return Ok(None);
    };
    match db {
        DbPool::Sqlite(pool) => {
            let row = sqlx::query(
                "SELECT id, username, password_hash, totp_enabled, totp_secret, display_name, locale, mark_read_policy FROM lyra_user WHERE id = ?",
            )
            .bind(&id)
            .fetch_optional(pool)
            .await
            .map_err(|e| {
                tracing::error!("DB error finding user by ID: {e}");
                AuthError::internal("Failed to look up user")
            })?;
            Ok(row.map(|r| UserData {
                id: id_from_row(&r, "id"),
                username: r.get("username"),
                password_hash: Some(r.get("password_hash")),
                totp_enabled: r.get("totp_enabled"),
                totp_secret: r.get("totp_secret"),
                display_name: r.get("display_name"),
                locale: r.get("locale"),
                mark_read_policy: parse_stored_mark_read_policy(r.get("mark_read_policy")),
            }))
        }
        #[cfg(feature = "postgres")]
        DbPool::Postgres(pool) => {
            let row = sqlx::query(
                "SELECT id, username, password_hash, totp_enabled, totp_secret, display_name, locale, mark_read_policy FROM lyra_user WHERE id = $1",
            )
            .bind(&id)
            .fetch_optional(pool)
            .await
            .map_err(|e| {
                tracing::error!("DB error finding user by ID: {e}");
                AuthError::internal("Failed to look up user")
            })?;
            Ok(row.map(|r| UserData {
                id: id_from_row(&r, "id"),
                username: r.get("username"),
                password_hash: Some(r.get("password_hash")),
                totp_enabled: r.get("totp_enabled"),
                totp_secret: r.get("totp_secret"),
                display_name: r.get("display_name"),
                locale: r.get("locale"),
                mark_read_policy: parse_stored_mark_read_policy(r.get("mark_read_policy")),
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
    let id = id_param(db, user_id).map_err(|_| StatusCode::NOT_FOUND)?;
    match db {
        DbPool::Sqlite(pool) => {
            sqlx::query(
                "UPDATE lyra_user SET totp_secret = ?, totp_enabled = ?, updated_at = datetime('now') WHERE id = ?",
            )
            .bind(totp_secret)
            .bind(totp_enabled)
            .bind(&id)
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
            .bind(&id)
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

async fn update_user_password(
    db: &DbPool,
    user_id: &str,
    password_hash: &str,
) -> Result<(), StatusCode> {
    let id = id_param(db, user_id).map_err(|_| StatusCode::NOT_FOUND)?;
    match db {
        DbPool::Sqlite(pool) => {
            sqlx::query(
                "UPDATE lyra_user SET password_hash = ?, updated_at = datetime('now') WHERE id = ?",
            )
            .bind(password_hash)
            .bind(&id)
            .execute(pool)
            .await
            .map_err(|e| {
                tracing::error!("Failed to update password: {e}");
                StatusCode::INTERNAL_SERVER_ERROR
            })?;
        }
        #[cfg(feature = "postgres")]
        DbPool::Postgres(pool) => {
            sqlx::query(
                "UPDATE lyra_user SET password_hash = $1, updated_at = NOW() WHERE id = $2",
            )
            .bind(password_hash)
            .bind(&id)
            .execute(pool)
            .await
            .map_err(|e| {
                tracing::error!("Failed to update password: {e}");
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

// ── Rate limiting (fixed window per key, via kv counters) ────────────

// Keyed per username: an attacker who knows the (single, v1) username can
// lock out the legit user for 15 minutes. That lockout-DoS tradeoff is
// deliberate for single-user v1 — the alternative (keying per IP) is
// trivially bypassed behind proxies and we have no trusted client IP yet.
fn login_rl_key(username: &str) -> String {
    format!("rl:login:{username}")
}

/// Keyed per user (not per pending token): re-logging in must not mint a
/// fresh allowance of TOTP attempts.
fn totp_rl_key(user_id: &str) -> String {
    format!("rl:totp:{user_id}")
}

/// Guards password-gated endpoints (change-password, TOTP disable) so a
/// stolen session is not an offline-speed password oracle.
fn pwd_rl_key(user_id: &str) -> String {
    format!("rl:pwd:{user_id}")
}

fn totp_step_key(user_id: &str) -> String {
    format!("totp_last_step:{user_id}")
}

/// True once `key` has hit `max_attempts` within its current window.
async fn is_rate_limited(
    kv: &dyn KvStore,
    key: &str,
    max_attempts: i64,
) -> Result<bool, StatusCode> {
    let attempts = kv
        .get(key)
        .await
        .map_err(|e| {
            tracing::error!("rate limit read failed: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(0);
    Ok(attempts >= max_attempts)
}

/// Count a failed attempt. The counter TTL is set on first failure, so the
/// fixed window starts then and restarts after expiry.
async fn record_failed_attempt(
    kv: &dyn KvStore,
    key: &str,
    window_secs: u64,
) -> Result<i64, StatusCode> {
    kv.incr(key, 1, Some(window_secs)).await.map_err(|e| {
        tracing::error!("rate limit incr failed: {e}");
        StatusCode::INTERNAL_SERVER_ERROR
    })
}

/// Reset the counter after a successful attempt.
async fn clear_failed_attempts(kv: &dyn KvStore, key: &str) {
    if let Err(e) = kv.del(key).await {
        tracing::warn!("rate limit clear failed: {e}");
    }
}

/// 429 when `key` is at the attempt cap; kv failures surface as 500 with `op`.
async fn ensure_not_rate_limited(kv: &dyn KvStore, key: &str, op: &str) -> Result<(), AuthError> {
    if is_rate_limited(kv, key, RATE_LIMIT_MAX_ATTEMPTS)
        .await
        .map_err(|_| AuthError::internal(op))?
    {
        return Err(AuthError::TooManyRequests);
    }
    Ok(())
}

/// Record a failed attempt; kv failures surface as 500 with `op`.
async fn note_failed_attempt(kv: &dyn KvStore, key: &str, op: &str) -> Result<(), AuthError> {
    record_failed_attempt(kv, key, RATE_LIMIT_WINDOW_SECS)
        .await
        .map_err(|_| AuthError::internal(op))?;
    Ok(())
}

async fn fetch_sess_epoch(db: &DbPool, user_id: &str) -> Result<i64, StatusCode> {
    let id = id_param(db, user_id).map_err(|_| StatusCode::NOT_FOUND)?;
    let epoch: i64 = match db {
        DbPool::Sqlite(pool) => sqlx::query_scalar("SELECT sess_epoch FROM lyra_user WHERE id = ?")
            .bind(&id)
            .fetch_optional(pool)
            .await
            .map_err(|e| {
                tracing::error!("fetch sess_epoch failed: {e}");
                StatusCode::INTERNAL_SERVER_ERROR
            })?
            .ok_or(StatusCode::NOT_FOUND)?,
        #[cfg(feature = "postgres")]
        DbPool::Postgres(pool) => {
            // PG INTEGER is INT4; sqlx i64 is INT8. Decode i32, widen.
            let epoch: i32 = sqlx::query_scalar("SELECT sess_epoch FROM lyra_user WHERE id = $1")
                .bind(&id)
                .fetch_optional(pool)
                .await
                .map_err(|e| {
                    tracing::error!("fetch sess_epoch failed: {e}");
                    StatusCode::INTERNAL_SERVER_ERROR
                })?
                .ok_or(StatusCode::NOT_FOUND)?;
            i64::from(epoch)
        }
    };
    Ok(epoch)
}

/// Bump `sess_epoch` and delete session keys for the previous epoch.
pub async fn invalidate_user_sessions(
    pool: &DbPool,
    kv: &dyn KvStore,
    user_id: &str,
) -> Result<(), StatusCode> {
    let old_epoch = fetch_sess_epoch(pool, user_id).await?;
    let id = id_param(pool, user_id).map_err(|_| StatusCode::NOT_FOUND)?;
    // Split per dialect so we do not unify SqliteQueryResult with PgQueryResult.
    db_execute!(
        pool,
        "UPDATE lyra_user SET sess_epoch = sess_epoch + 1 WHERE id = ?",
        &id
    )
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
    pub fn kv(&self) -> &Arc<dyn KvStore> {
        &self.kv
    }

    pub async fn create_session(&self, user_id: &str) -> Result<String, StatusCode> {
        let epoch = fetch_sess_epoch(&self.db, user_id).await?;
        let token = generate_token();
        self.kv
            .set(&sess_key(epoch, &token), user_id, Some(SESSION_TTL_SECS))
            .await
            .map_err(|e| {
                tracing::error!("session set failed: {e}");
                StatusCode::INTERNAL_SERVER_ERROR
            })?;
        // Reverse index so get_session can resolve token → user, then check current epoch.
        self.kv
            .set(&tok_key(&token), user_id, Some(SESSION_TTL_SECS))
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
            .set(&pending_key(&token), user_id, Some(PENDING_TTL_SECS))
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
static MASTER_KEY: std::sync::OnceLock<Zeroizing<Vec<u8>>> = std::sync::OnceLock::new();

/// Install the master key. The first install wins; later calls are no-ops
/// (tests share one process-wide key). A second install with a *different*
/// key is suspicious, so it is logged.
pub(crate) fn install_master_key(key: &[u8]) {
    if MASTER_KEY.set(Zeroizing::new(key.to_vec())).is_err()
        && let Some(existing) = MASTER_KEY.get()
        && existing.as_slice() != key
    {
        tracing::warn!(
            "install_master_key called again with a different key; keeping the first (first install wins)"
        );
    }
}

fn master_key() -> Result<&'static [u8], CryptoError> {
    MASTER_KEY
        .get()
        .map(|k| k.as_slice())
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
    let id = id_param(db, user_id).map_err(|e| CryptoError::Storage(e.to_string()))?;
    let wrapped: Option<String> = match db {
        DbPool::Sqlite(pool) => {
            sqlx::query_scalar("SELECT encrypted_dek FROM lyra_user WHERE id = ?")
                .bind(&id)
                .fetch_optional(pool)
                .await
        }
        #[cfg(feature = "postgres")]
        DbPool::Postgres(pool) => {
            sqlx::query_scalar("SELECT encrypted_dek FROM lyra_user WHERE id = $1")
                .bind(&id)
                .fetch_optional(pool)
                .await
        }
    }
    .map_err(|e| CryptoError::Storage(e.to_string()))?
    .flatten();
    wrapped.ok_or(CryptoError::MissingDek)
}

/// Persist a wrapped DEK only when the user exists and has none yet.
async fn store_wrapped_dek_if_missing(
    db: &DbPool,
    user_id: &str,
    wrapped: &str,
) -> Result<u64, CryptoError> {
    let id = id_param(db, user_id).map_err(|e| CryptoError::Storage(e.to_string()))?;
    db_execute!(
        db,
        "UPDATE lyra_user SET encrypted_dek = ?, updated_at = datetime('now') \
         WHERE id = ? AND encrypted_dek IS NULL",
        wrapped,
        &id
    )
    .map_err(|e| CryptoError::Storage(e.to_string()))
}

/// Mint a DEK for a pre-hierarchy user and re-encrypt secrets that still use
/// the padded-master-key scheme.
async fn provision_and_rotate_legacy_dek(
    db: &DbPool,
    user_id: &str,
    kek: &[u8; 32],
) -> Result<Vec<u8>, CryptoError> {
    let dek = crypto::generate_key();
    let wrapped = crypto::wrap_dek(kek, &dek)?;
    let wrote = store_wrapped_dek_if_missing(db, user_id, &wrapped).await?;
    if wrote == 0 {
        // Unknown user, or another caller stored a DEK first.
        let existing = fetch_encrypted_dek(db, user_id).await?;
        return crypto::unwrap_dek(kek, &existing);
    }
    if let Err(e) = rotate_legacy_secrets(db, user_id, &dek).await {
        tracing::error!(error = %e, %user_id, "failed to rotate legacy secrets onto the new DEK");
        return Err(e);
    }
    Ok(dek.to_vec())
}

async fn rotate_legacy_secrets(db: &DbPool, user_id: &str, dek: &[u8]) -> Result<(), CryptoError> {
    let master = master_key()?;
    let accounts = fetch_account_credentials(db, user_id).await?;
    for (account_id, credential) in accounts {
        let Ok(blob) = serde_json::from_str::<crypto::EncryptedCredential>(&credential) else {
            continue;
        };
        let Some(plaintext) = crypto::try_decrypt_with_legacy_keys(&blob, master) else {
            continue;
        };
        let plaintext = Zeroizing::new(plaintext);
        let rotated = crypto::encrypt(dek, &plaintext)?;
        let json =
            serde_json::to_string(&rotated).map_err(|e| CryptoError::Encrypt(e.to_string()))?;
        update_account_credential(db, &account_id, &json).await?;
    }
    rotate_legacy_totp_secret(db, user_id, dek, master).await?;
    Ok(())
}

async fn fetch_account_credentials(
    db: &DbPool,
    user_id: &str,
) -> Result<Vec<(String, String)>, CryptoError> {
    let id = id_param(db, user_id).map_err(|e| CryptoError::Storage(e.to_string()))?;
    db_fetch_all!(
        db,
        "SELECT id, credential FROM mail_account WHERE user_id = ?",
        |row| (id_from_row(row, "id"), row.get::<String, _>("credential")),
        &id
    )
    .map_err(|e| CryptoError::Storage(e.to_string()))
}

async fn update_account_credential(
    db: &DbPool,
    account_id: &str,
    credential: &str,
) -> Result<(), CryptoError> {
    let id = id_param(db, account_id).map_err(|e| CryptoError::Storage(e.to_string()))?;
    db_execute!(
        db,
        "UPDATE mail_account SET credential = ?, updated_at = datetime('now') WHERE id = ?",
        credential,
        &id
    )
    .map_err(|e| CryptoError::Storage(e.to_string()))?;
    Ok(())
}

async fn rotate_legacy_totp_secret(
    db: &DbPool,
    user_id: &str,
    dek: &[u8],
    master: &[u8],
) -> Result<(), CryptoError> {
    let id = id_param(db, user_id).map_err(|e| CryptoError::Storage(e.to_string()))?;
    let stored: Option<Option<String>> = db_scalar_optional!(
        db,
        Option<String>,
        "SELECT totp_secret FROM lyra_user WHERE id = ?",
        &id
    )
    .map_err(|e| CryptoError::Storage(e.to_string()))?;
    let Some(stored) = stored.flatten() else {
        return Ok(());
    };
    let plaintext = if let Ok(blob) = serde_json::from_str::<crypto::EncryptedCredential>(&stored) {
        let Some(pt) = crypto::try_decrypt_with_legacy_keys(&blob, master) else {
            return Ok(());
        };
        Zeroizing::new(String::from_utf8_lossy(&pt).into_owned())
    } else {
        Zeroizing::new(stored)
    };
    let rotated = encrypt_totp_secret(dek, &plaintext)?;
    db_execute!(
        db,
        "UPDATE lyra_user SET totp_secret = ? WHERE id = ?",
        &rotated,
        &id
    )
    .map_err(|e| CryptoError::Storage(e.to_string()))?;
    Ok(())
}

/// Map a crypto failure to a 500. The error text carries deliberate operator
/// guidance (never key material or plaintext).
fn crypto_err(e: CryptoError) -> AuthError {
    tracing::error!("crypto failure: {e}");
    AuthError::Crypto(e)
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

    /// Kv store for sessions and user settings (privacy allow-list, etc.).
    pub fn kv(&self) -> &Arc<dyn KvStore> {
        self.sessions.kv()
    }

    /// Get the user's data encryption key (DEK) for credential encryption.
    ///
    /// The DEK is a random 256-bit key generated at bootstrap, wrapped with
    /// the per-user KEK (HKDF-SHA256 from the master key, bound to the user
    /// id) and stored in `lyra_user.encrypted_dek`. This unwraps it on demand.
    ///
    /// Users created before the DEK hierarchy have a NULL `encrypted_dek`.
    /// The first lookup mints a DEK and re-encrypts account passwords that
    /// still use the old padded-`LYRA_MASTER_KEY` scheme.
    ///
    /// # Errors
    /// Fails with a typed [`CryptoError`] if the master key was never
    /// installed, the user row is missing, or unwrapping fails (e.g. the
    /// DEK was wrapped under a different master key — re-add accounts or
    /// reset the database).
    pub async fn get_user_dek(db: &DbPool, user_id: &str) -> Result<Vec<u8>, CryptoError> {
        let kek = crypto::derive_user_kek(master_key()?, user_id);
        match fetch_encrypted_dek(db, user_id).await {
            Ok(wrapped) => crypto::unwrap_dek(&kek, &wrapped),
            Err(CryptoError::MissingDek) => {
                provision_and_rotate_legacy_dek(db, user_id, &kek).await
            }
            Err(e) => Err(e),
        }
    }

    /// Unwrap the user DEK (minting/rotating legacy credentials if needed),
    /// then reload the account password blob so callers never decrypt a
    /// pre-rotation snapshot.
    pub async fn get_user_dek_and_credential(
        db: &DbPool,
        user_id: &str,
        account_id: &str,
    ) -> Result<(Vec<u8>, String), CryptoError> {
        let dek = Self::get_user_dek(db, user_id).await?;
        let account = id_param(db, account_id).map_err(|e| CryptoError::Storage(e.to_string()))?;
        let user = id_param(db, user_id).map_err(|e| CryptoError::Storage(e.to_string()))?;
        let credential: Option<String> = db_scalar_optional!(
            db,
            String,
            "SELECT credential FROM mail_account WHERE id = ? AND user_id = ?",
            &account,
            &user
        )
        .map_err(|e| CryptoError::Storage(e.to_string()))?;
        let credential = credential.ok_or_else(|| {
            CryptoError::Storage("mail account not found while loading credentials".into())
        })?;
        Ok((dek, credential))
    }
}

// ── Route handlers ──────────────────────────────────────────────────

/// Public auth routes (no middleware).
pub fn routes() -> Router<AuthState> {
    Router::new()
        .route("/api/v1/auth/status", get(auth_status))
        .route("/api/v1/auth/bootstrap", post(auth_bootstrap))
        .route("/api/v1/auth/login", post(auth_login))
        .route("/api/v1/auth/logout", post(auth_logout))
        .route("/api/v1/auth/me", get(auth_me))
        .route("/api/v1/auth/preferences", patch(patch_preferences))
        .route("/api/v1/auth/change-password", post(change_password))
        .route("/api/v1/auth/totp/enroll", post(totp_enroll))
        .route("/api/v1/auth/totp/confirm", post(totp_enroll_confirm))
        .route("/api/v1/auth/totp/verify", post(totp_verify))
        .route("/api/v1/auth/totp/disable", post(totp_disable))
}

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
) -> Result<impl IntoResponse, AuthError> {
    // Fast-path UX check; the real guard is the `singleton` unique index on
    // lyra_user (migration 0005), which rejects a second row even when two
    // concurrent bootstraps both pass this check.
    if has_any_user(&state.db).await.unwrap_or(false) {
        return Err(AuthError::Conflict(BOOTSTRAP_TAKEN.to_string()));
    }

    if req.username.is_empty() || req.username.len() > 64 {
        return Err(AuthError::BadRequest(
            "Username must be between 1 and 64 characters".to_string(),
        ));
    }

    if let Err(msg) = validate_password(&req.password, state.min_password_length) {
        return Err(AuthError::BadRequest(msg));
    }

    let password_hash = hash_password(&req.password).await?;

    let user_id = Uuid::new_v7(uuid::Timestamp::now(uuid::NoContext)).to_string();
    let locale = req.locale.unwrap_or_else(|| "en".to_string());

    // Generate the user's DEK and store it wrapped with the per-user KEK.
    let dek = crypto::generate_key();
    let kek = crypto::derive_user_kek(master_key().map_err(crypto_err)?, &user_id);
    let wrapped_dek = crypto::wrap_dek(&kek, &dek).map_err(crypto_err)?;

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
        if is_unique_violation(&e) {
            // Lost the bootstrap race (singleton guard) or username taken.
            AuthError::Conflict(BOOTSTRAP_TAKEN.to_string())
        } else {
            tracing::error!("Failed to insert user: {e}");
            AuthError::internal("Failed to create user")
        }
    })?;

    let token = state
        .sessions
        .create_session(&user_id)
        .await
        .map_err(|_| AuthError::internal("Failed to create session"))?;

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
                mark_read_policy: "on_open".to_string(),
            },
            requires_totp: false,
        }),
    ))
}

async fn auth_login(
    State(state): State<AuthState>,
    Json(req): Json<LoginRequest>,
) -> Result<Json<LoginResponse>, AuthError> {
    let kv = Arc::clone(state.sessions.kv());
    let rl_key = login_rl_key(&req.username);
    ensure_not_rate_limited(kv.as_ref(), &rl_key, "Authentication failed").await?;

    let Some(user) = find_user_by_username(&state.db, &req.username).await? else {
        note_failed_attempt(kv.as_ref(), &rl_key, "Authentication failed").await?;
        return Err(AuthError::invalid_credentials());
    };

    let valid = verify_password(
        &req.password,
        user.password_hash
            .as_deref()
            .ok_or_else(|| AuthError::internal("Password hash not available"))?,
    )
    .await
    .map_err(|_| AuthError::internal("Authentication failed"))?;

    if !valid {
        note_failed_attempt(kv.as_ref(), &rl_key, "Authentication failed").await?;
        return Err(AuthError::invalid_credentials());
    }

    // Password correct: reset the failed-attempt counter for this username.
    clear_failed_attempts(kv.as_ref(), &rl_key).await;

    if user.totp_enabled {
        let pending_token = state
            .sessions
            .create_pending_session(&user.id)
            .await
            .map_err(|_| AuthError::internal("Failed to create pending session"))?;
        return Ok(Json(LoginResponse {
            token: pending_token,
            user: user_info_from(&user),
            requires_totp: true,
        }));
    }

    let token = state
        .sessions
        .create_session(&user.id)
        .await
        .map_err(|_| AuthError::internal("Failed to create session"))?;

    Ok(Json(LoginResponse {
        token,
        user: user_info_from(&user),
        requires_totp: false,
    }))
}

async fn totp_verify(
    State(state): State<AuthState>,
    Json(req): Json<TotpVerifyRequest>,
) -> Result<Json<LoginResponse>, AuthError> {
    let kv = Arc::clone(state.sessions.kv());
    // Resolve the pending session first so the limiter keys on the user, not
    // the token — otherwise re-login would mint a fresh allowance.
    let user_id = state
        .sessions
        .get_pending_session(&req.pending_token)
        .await
        .ok_or_else(|| AuthError::unauthorized("Invalid or expired pending session"))?;

    let rl_key = totp_rl_key(&user_id);
    ensure_not_rate_limited(kv.as_ref(), &rl_key, "Verification failed").await?;

    let user = find_user_by_id(&state.db, &user_id)
        .await?
        .ok_or_else(|| AuthError::internal("User not found"))?;

    verify_totp_code(&state, kv.as_ref(), &rl_key, &user_id, &user, &req.code).await?;

    let token = state
        .sessions
        .promote_pending_session(&req.pending_token)
        .await
        .map_err(|_| AuthError::internal("Failed to promote session"))?
        .ok_or_else(|| AuthError::unauthorized("Invalid or expired pending session"))?;

    Ok(Json(LoginResponse {
        token,
        user: user_info_from(&user),
        requires_totp: false,
    }))
}

/// Verify a login TOTP code for `user`, with per-user rate limiting and
/// replay protection (last accepted timestep in kv).
async fn verify_totp_code(
    state: &AuthState,
    kv: &dyn KvStore,
    rl_key: &str,
    user_id: &str,
    user: &UserData,
    code: &str,
) -> Result<(), AuthError> {
    let stored_secret = user
        .totp_secret
        .as_deref()
        .ok_or_else(|| AuthError::internal("TOTP not configured"))?;
    let dek = AuthState::get_user_dek(&state.db, user_id)
        .await
        .map_err(crypto_err)?;
    let secret = Zeroizing::new(decrypt_totp_secret(&dek, stored_secret).map_err(crypto_err)?);
    let totp = build_totp(&secret, &user.username)?;

    let Some(step) = matched_totp_step(&totp, code) else {
        note_failed_attempt(kv, rl_key, "Verification failed").await?;
        return Err(AuthError::unauthorized("Invalid TOTP code"));
    };

    // Replay guard: a code at or below the last accepted timestep was already
    // used (codes stay valid for ±1 step, so the ±1 skew allows reuse).
    let step_key = totp_step_key(user_id);
    let last_step: Option<u64> = kv
        .get(&step_key)
        .await
        .ok()
        .flatten()
        .and_then(|v| v.parse().ok());
    if last_step.is_some_and(|s| step <= s) {
        note_failed_attempt(kv, rl_key, "Verification failed").await?;
        return Err(AuthError::unauthorized("TOTP code already used"));
    }
    let step_value = step.to_string();
    kv.set(&step_key, &step_value, Some(TOTP_LAST_STEP_TTL_SECS))
        .await
        .map_err(|e| {
            tracing::error!("totp step store failed: {e}");
            AuthError::internal("Verification failed")
        })?;
    clear_failed_attempts(kv, rl_key).await;
    Ok(())
}

async fn totp_enroll(
    State(state): State<AuthState>,
    AuthUser(user_id): AuthUser,
) -> Result<Json<TotpEnrollResponse>, AuthError> {
    let user = find_user_by_id(&state.db, &user_id)
        .await?
        .ok_or_else(|| AuthError::internal("User not found"))?;

    // Re-enrolling while 2FA is active would silently rotate the secret;
    // the user must disable first (which requires the password).
    if user.totp_enabled {
        return Err(AuthError::Conflict(
            "TOTP is already enabled. Disable it before re-enrolling.".to_string(),
        ));
    }

    let secret_bytes =
        Zeroizing::new(totp_rs::Secret::generate_secret().to_bytes().map_err(|e| {
            tracing::error!("Failed to generate TOTP secret: {e}");
            AuthError::internal("Failed to generate TOTP secret")
        })?);

    let secret_base32 = base32::encode(base32::Alphabet::Rfc4648 { padding: false }, &secret_bytes);
    // Store the secret encrypted with the user's DEK; enabled only after confirm.
    let dek = AuthState::get_user_dek(&state.db, &user_id)
        .await
        .map_err(crypto_err)?;
    let stored_secret = encrypt_totp_secret(&dek, &secret_base32).map_err(crypto_err)?;
    update_user_totp(&state.db, &user_id, Some(&stored_secret), false)
        .await
        .map_err(|_| AuthError::internal("Failed to store TOTP secret"))?;

    let totp = build_totp_from_raw(&secret_bytes, &user.username)?;

    Ok(Json(TotpEnrollResponse {
        secret: secret_base32,
        otpauth_uri: totp.get_url(),
    }))
}

async fn totp_enroll_confirm(
    State(state): State<AuthState>,
    AuthUser(user_id): AuthUser,
    Json(req): Json<TotpEnrollConfirmRequest>,
) -> Result<Json<AuthStatus>, AuthError> {
    let user = find_user_by_id(&state.db, &user_id)
        .await?
        .ok_or_else(|| AuthError::internal("User not found"))?;

    let stored = user
        .totp_secret
        .ok_or_else(|| AuthError::BadRequest("TOTP enrollment not started".to_string()))?;

    let dek = AuthState::get_user_dek(&state.db, &user_id)
        .await
        .map_err(crypto_err)?;
    let secret = Zeroizing::new(decrypt_totp_secret(&dek, &stored).map_err(crypto_err)?);

    let totp = build_totp(&secret, &user.username)?;

    if !totp.check_current(&req.code).unwrap_or(false) {
        return Err(AuthError::unauthorized(
            "Invalid TOTP code. Please try again.",
        ));
    }

    // Code verified: keep the same encrypted secret, flip the enabled flag.
    update_user_totp(&state.db, &user_id, Some(&stored), true)
        .await
        .map_err(|_| AuthError::internal("Failed to enable TOTP"))?;

    Ok(Json(AuthStatus {
        has_user: true,
        totp_enabled: true,
    }))
}

async fn totp_disable(
    State(state): State<AuthState>,
    AuthUser(user_id): AuthUser,
    Json(req): Json<TotpDisableRequest>,
) -> Result<Json<AuthStatus>, AuthError> {
    // Disabling 2FA weakens account security, so it requires re-authentication
    // with the current password, not just a session token. The password check
    // is rate-limited per user so a stolen session is no password oracle.
    let kv = Arc::clone(state.sessions.kv());
    let rl_key = pwd_rl_key(&user_id);
    ensure_not_rate_limited(kv.as_ref(), &rl_key, "Verification failed").await?;

    let user = find_user_by_id(&state.db, &user_id)
        .await?
        .ok_or_else(|| AuthError::internal("User not found"))?;

    let valid = verify_password(
        &req.password,
        &user
            .password_hash
            .ok_or_else(|| AuthError::internal("Password hash not available"))?,
    )
    .await
    .map_err(|_| AuthError::internal("Verification failed"))?;
    if !valid {
        note_failed_attempt(kv.as_ref(), &rl_key, "Verification failed").await?;
        return Err(AuthError::unauthorized("Invalid password"));
    }
    clear_failed_attempts(kv.as_ref(), &rl_key).await;

    update_user_totp(&state.db, &user_id, None, false)
        .await
        .map_err(|_| AuthError::internal("Failed to disable TOTP"))?;

    Ok(Json(AuthStatus {
        has_user: true,
        totp_enabled: false,
    }))
}

async fn change_password(
    State(state): State<AuthState>,
    AuthUser(user_id): AuthUser,
    Json(req): Json<ChangePasswordRequest>,
) -> Result<StatusCode, AuthError> {
    // The current-password check is rate-limited per user so a stolen session
    // is no offline-speed password oracle.
    let kv = Arc::clone(state.sessions.kv());
    let rl_key = pwd_rl_key(&user_id);
    ensure_not_rate_limited(kv.as_ref(), &rl_key, "Verification failed").await?;

    let user = find_user_by_id(&state.db, &user_id)
        .await?
        .ok_or_else(|| AuthError::internal("User not found"))?;

    let valid = verify_password(
        &req.current_password,
        &user
            .password_hash
            .ok_or_else(|| AuthError::internal("Password hash not available"))?,
    )
    .await
    .map_err(|_| AuthError::internal("Verification failed"))?;
    if !valid {
        note_failed_attempt(kv.as_ref(), &rl_key, "Verification failed").await?;
        return Err(AuthError::unauthorized("Current password is incorrect"));
    }
    clear_failed_attempts(kv.as_ref(), &rl_key).await;

    if let Err(msg) = validate_password(&req.new_password, state.min_password_length) {
        return Err(AuthError::BadRequest(msg));
    }

    let new_hash = hash_password(&req.new_password).await?;
    update_user_password(&state.db, &user_id, &new_hash)
        .await
        .map_err(|_| AuthError::internal("Failed to update password"))?;

    // Kick every session, including the caller's: after a password change all
    // clients must log in again with the new password. (Simplest and safest —
    // the caller's session is not special-cased.)
    invalidate_user_sessions(&state.db, state.sessions.kv().as_ref(), &user_id)
        .await
        .map_err(|_| AuthError::internal("Failed to invalidate sessions"))?;

    // Also clear any login lockout for this username, so the legit user is
    // not stuck behind an attacker's failed-attempt window.
    clear_failed_attempts(kv.as_ref(), &login_rl_key(&user.username)).await;

    Ok(StatusCode::NO_CONTENT)
}

async fn auth_logout(
    State(state): State<AuthState>,
    headers: HeaderMap,
) -> Result<StatusCode, AuthError> {
    if let Some(token) = extract_token_from_headers(&headers) {
        state.sessions.remove_session(&token).await;
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn auth_me(
    State(state): State<AuthState>,
    AuthUser(user_id): AuthUser,
) -> Result<Json<UserInfo>, AuthError> {
    let user = find_user_by_id(&state.db, &user_id)
        .await?
        .ok_or_else(|| AuthError::internal("User not found"))?;

    Ok(Json(user_info_from(&user)))
}

async fn patch_preferences(
    State(state): State<AuthState>,
    AuthUser(user_id): AuthUser,
    Json(req): Json<PreferencesRequest>,
) -> Result<Json<UserInfo>, AuthError> {
    let Some(raw) = req.mark_read_policy else {
        return Err(AuthError::BadRequest("provide markReadPolicy".into()));
    };
    let policy = parse_mark_read_policy(&raw)?;
    update_mark_read_policy(&state.db, &user_id, &policy).await?;
    let user = find_user_by_id(&state.db, &user_id)
        .await?
        .ok_or_else(|| AuthError::internal("User not found"))?;
    Ok(Json(user_info_from(&user)))
}

async fn update_mark_read_policy(
    db: &DbPool,
    user_id: &str,
    policy: &str,
) -> Result<(), AuthError> {
    let id = id_param(db, user_id).map_err(|_| AuthError::internal("Failed to look up user"))?;
    db_execute!(
        db,
        "UPDATE lyra_user SET mark_read_policy = ?, updated_at = datetime('now') WHERE id = ?",
        policy,
        &id
    )
    .map_err(|e| {
        tracing::error!("DB error updating mark_read_policy: {e}");
        AuthError::internal("Failed to update preferences")
    })?;
    Ok(())
}

// ── Helpers ─────────────────────────────────────────────────────────

fn extract_token_from_headers(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(String::from)
}

async fn extract_session(state: &AuthState, headers: &HeaderMap) -> Result<String, AuthError> {
    let token = extract_token_from_headers(headers)
        .ok_or_else(|| AuthError::unauthorized("Missing authorization header"))?;

    state
        .sessions
        .get_session(&token)
        .await
        .ok_or_else(|| AuthError::unauthorized("Invalid or expired session"))
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

fn build_totp(secret_base32: &str, username: &str) -> Result<TOTP, AuthError> {
    let secret_bytes = base32::decode(base32::Alphabet::Rfc4648 { padding: false }, secret_base32)
        .ok_or_else(|| {
            tracing::error!("Invalid TOTP secret encoding");
            AuthError::internal("Failed to build TOTP")
        })?;
    build_totp_from_raw(&secret_bytes, username)
}

fn build_totp_from_raw(secret_bytes: &[u8], username: &str) -> Result<TOTP, AuthError> {
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
        AuthError::internal("Failed to build TOTP")
    })
}

/// Find the timestep at which `code` is valid (current ± 1 step, matching the
/// skew of `TOTP::check_current`) so replay protection can compare steps.
/// Returns `None` when the code matches no step in the window.
fn matched_totp_step(totp: &TOTP, code: &str) -> Option<u64> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    let step = totp.step;
    let current = now / step;
    (current.saturating_sub(1)..=current + 1).find(|&s| totp.generate(s * step) == code)
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

    #[tokio::test]
    async fn password_hashing_roundtrip() {
        let password = "TestPassw0rd!";
        let hash = hash_password(password).await.unwrap();
        assert!(verify_password(password, &hash).await.unwrap());
        assert!(!verify_password("WrongPassw0rd!", &hash).await.unwrap());
    }

    #[test]
    fn install_master_key_first_wins() {
        install_master_key(TEST_MASTER_KEY);
        install_master_key(b"different-key-that-is-also-32-bytes!!");
        assert_eq!(master_key().unwrap(), TEST_MASTER_KEY);
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

        let epoch_before = fetch_sess_epoch(&db, "user-1").await.unwrap();
        invalidate_user_sessions(&db, kv.as_ref(), "user-1")
            .await
            .unwrap();
        let epoch_after = fetch_sess_epoch(&db, "user-1").await.unwrap();
        assert_eq!(epoch_after, epoch_before + 1);

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
        // The single-user guard (migration 0005) forbids two rows in one
        // database, so each user lives in its own in-memory DB here.
        let db_a = test_pool().await;
        let db_b = test_pool().await;
        let dek_a = seed_user_with_dek(&db_a, "user-a").await;
        let dek_b = seed_user_with_dek(&db_b, "user-b").await;

        assert_eq!(
            AuthState::get_user_dek(&db_a, "user-a").await.unwrap(),
            dek_a
        );
        assert_eq!(
            AuthState::get_user_dek(&db_b, "user-b").await.unwrap(),
            dek_b
        );
        assert_ne!(dek_a, dek_b);

        // A user's wrapped DEK cannot be unwrapped with another user's KEK.
        let stored_a: String =
            sqlx::query_scalar("SELECT encrypted_dek FROM lyra_user WHERE id = 'user-a'")
                .fetch_one(sqlite_pool(&db_a))
                .await
                .unwrap();
        let kek_b = crypto::derive_user_kek(TEST_MASTER_KEY, "user-b");
        assert!(crypto::unwrap_dek(&kek_b, &stored_a).is_err());
    }

    #[tokio::test]
    async fn bootstrap_rejects_second_user_with_conflict() {
        let db = test_pool().await;
        let state = test_state(db.clone());
        bootstrap_alice(&state).await;

        let err = auth_bootstrap(
            State(state),
            Json(BootstrapRequest {
                username: "bob".into(),
                password: "Str0ngPass1".into(),
                display_name: None,
                locale: None,
            }),
        )
        .await
        .err()
        .expect("second bootstrap must fail");
        assert_eq!(err.status(), StatusCode::CONFLICT);

        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM lyra_user")
            .fetch_one(sqlite_pool(&db))
            .await
            .unwrap();
        assert_eq!(count, 1, "exactly one user may exist");
    }

    #[tokio::test]
    async fn second_user_insert_violates_singleton_guard() {
        let db = test_pool().await;
        insert_user(&db, "id-1", "alice", "hash", None, "en", "dek")
            .await
            .unwrap();

        // A second row with a *different* username must be rejected by the
        // singleton unique index, not merely by UNIQUE(username).
        let err = insert_user(&db, "id-2", "bob", "hash", None, "en", "dek")
            .await
            .unwrap_err();
        assert!(
            is_unique_violation(&err),
            "expected a unique violation, got {err}"
        );

        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM lyra_user")
            .fetch_one(sqlite_pool(&db))
            .await
            .unwrap();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn unknown_user_dek_is_a_typed_error() {
        let db = test_pool().await;
        install_test_master_key();
        let err = AuthState::get_user_dek(&db, "no-such-user")
            .await
            .unwrap_err();
        assert!(matches!(err, CryptoError::MissingDek));
    }

    #[tokio::test]
    async fn legacy_user_without_dek_rotates_padded_master_key_credentials() {
        let db = test_pool().await;
        install_test_master_key();
        seed_user(&db, "user-legacy").await;

        // Pre-DEK accounts were encrypted with the first 32 bytes of
        // LYRA_MASTER_KEY (or the old hardcoded default).
        let mut legacy_key = [0u8; 32];
        let default = b"lyra-default-master-key-for-dev-only";
        legacy_key[..32].copy_from_slice(&default[..32]);
        let encrypted = crypto::encrypt(&legacy_key, b"imap-secret-pass").unwrap();
        let credential_json = serde_json::to_string(&encrypted).unwrap();
        sqlx::query(
            r"
            INSERT INTO mail_account (
                id, user_id, display_name, email_address, protocol, auth_type,
                credential, imap_host, imap_port, imap_security,
                is_active, sync_enabled
            ) VALUES ('acct-1', 'user-legacy', 'Test', 'test@example.com',
                      'imap', 'password', ?, 'imap.example.com', 993, 'tls', 1, 1)
            ",
        )
        .bind(&credential_json)
        .execute(sqlite_pool(&db))
        .await
        .unwrap();

        let dek = AuthState::get_user_dek(&db, "user-legacy").await.unwrap();
        assert_eq!(dek.len(), 32);

        // In-memory blobs loaded *before* get_user_dek are stale.
        let stale: crypto::EncryptedCredential = serde_json::from_str(&credential_json).unwrap();
        assert!(
            crypto::decrypt(&dek, &stale).is_err(),
            "decrypting a pre-rotation blob with the new DEK must fail"
        );

        let (dek2, reloaded) = AuthState::get_user_dek_and_credential(&db, "user-legacy", "acct-1")
            .await
            .unwrap();
        assert_eq!(dek2, dek);

        let stored: String =
            sqlx::query_scalar("SELECT encrypted_dek FROM lyra_user WHERE id = 'user-legacy'")
                .fetch_one(sqlite_pool(&db))
                .await
                .unwrap();
        assert!(!stored.is_empty());

        let blob: crypto::EncryptedCredential = serde_json::from_str(&reloaded).unwrap();
        assert_eq!(crypto::decrypt(&dek, &blob).unwrap(), b"imap-secret-pass");
        assert!(
            crypto::decrypt(&legacy_key, &blob).is_err(),
            "credentials must not remain under the legacy padded key"
        );
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

        // Enroll: returns the plaintext secret once, stores only ciphertext.
        let enroll = totp_enroll(State(state.clone()), AuthUser(user_id.clone()))
            .await
            .unwrap();
        let plaintext_secret = enroll.secret.clone();

        let stored: String = sqlx::query_scalar("SELECT totp_secret FROM lyra_user WHERE id = ?")
            .bind(&user_id)
            .fetch_one(sqlite_pool(&db))
            .await
            .unwrap();
        assert_ne!(stored, plaintext_secret);
        assert!(!stored.contains(&plaintext_secret));
        serde_json::from_str::<crypto::EncryptedCredential>(&stored).unwrap();
        let enabled: bool = sqlx::query_scalar("SELECT totp_enabled FROM lyra_user WHERE id = ?")
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
            AuthUser(user_id.clone()),
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

    // ── Session TTLs, rate limiting, password change, 2FA hardening ──

    /// Seed a user with a known username/password and TOTP already enabled.
    /// Returns the plaintext base32 TOTP secret.
    async fn seed_user_with_totp(db: &DbPool, id: &str, username: &str, password: &str) -> String {
        seed_user_with_dek(db, id).await;
        let hash = hash_password(password).await.unwrap();
        let secret_bytes = totp_rs::Secret::generate_secret().to_bytes().unwrap();
        let secret_b32 =
            base32::encode(base32::Alphabet::Rfc4648 { padding: false }, &secret_bytes);
        let dek = AuthState::get_user_dek(db, id).await.unwrap();
        let stored = encrypt_totp_secret(&dek, &secret_b32).unwrap();
        sqlx::query(
            "UPDATE lyra_user SET username = ?, password_hash = ?, totp_secret = ?, totp_enabled = 1 WHERE id = ?",
        )
        .bind(username)
        .bind(hash)
        .bind(stored)
        .bind(id)
        .execute(sqlite_pool(db))
        .await
        .unwrap();
        secret_b32
    }

    /// Bootstrap user "alice" / "Str0ngPass1" and return a session token.
    async fn bootstrap_alice(state: &AuthState) -> String {
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
        let user_id: String =
            sqlx::query_scalar("SELECT id FROM lyra_user WHERE username = 'alice'")
                .fetch_one(sqlite_pool(state.db()))
                .await
                .unwrap();
        state.sessions.create_session(&user_id).await.unwrap()
    }

    #[test]
    fn password_max_length() {
        // 1024 chars passes, 1025 is rejected (Argon2 DoS cap).
        let ok = format!("Aa1{}", "a".repeat(1021));
        assert_eq!(ok.len(), 1024);
        assert!(validate_password(&ok, 8).is_ok());
        let too_long = format!("Aa1{}", "a".repeat(1022));
        assert_eq!(too_long.len(), 1025);
        assert!(validate_password(&too_long, 8).is_err());
    }

    #[tokio::test]
    async fn session_entries_carry_a_ttl() {
        let db = test_pool().await;
        seed_user(&db, "user-1").await;
        let kv = MemoryKv::new();
        let store = SessionStore::new(db.clone(), Arc::new(kv.clone()));
        let token = store.create_session("user-1").await.unwrap();
        let epoch = fetch_sess_epoch(&db, "user-1").await.unwrap();

        let sess_ttl = kv.ttl_remaining(&sess_key(epoch, &token)).await;
        let tok_ttl = kv.ttl_remaining(&tok_key(&token)).await;
        assert!(sess_ttl.is_some(), "session entry must have a TTL");
        assert!(tok_ttl.is_some(), "token index must have a TTL");
        // 7-day TTL, minus slack for test execution time.
        let floor = std::time::Duration::from_secs(SESSION_TTL_SECS - 60);
        assert!(sess_ttl.unwrap() > floor);
        assert!(tok_ttl.unwrap() > floor);
    }

    #[tokio::test]
    async fn pending_session_has_short_ttl() {
        let db = test_pool().await;
        seed_user(&db, "user-1").await;
        let kv = MemoryKv::new();
        let store = SessionStore::new(db, Arc::new(kv.clone()));
        let token = store.create_pending_session("user-1").await.unwrap();
        let ttl = kv.ttl_remaining(&pending_key(&token)).await;
        assert!(ttl.is_some(), "pending token must have a TTL");
        assert!(ttl.unwrap() <= std::time::Duration::from_secs(PENDING_TTL_SECS));
    }

    #[tokio::test]
    async fn rate_limit_counter_window_expires() {
        let kv = MemoryKv::new();
        for _ in 0..RATE_LIMIT_MAX_ATTEMPTS {
            record_failed_attempt(&kv, "rl:test", 1).await.unwrap();
        }
        assert!(
            is_rate_limited(&kv, "rl:test", RATE_LIMIT_MAX_ATTEMPTS)
                .await
                .unwrap()
        );
        // Window of 1s expires; the counter resets and attempts are allowed again.
        tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
        assert!(
            !is_rate_limited(&kv, "rl:test", RATE_LIMIT_MAX_ATTEMPTS)
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn login_rate_limited_after_five_failures() {
        let db = test_pool().await;
        let state = test_state(db);
        bootstrap_alice(&state).await;

        let bad_login = |password: &str| LoginRequest {
            username: "alice".into(),
            password: password.into(),
        };
        for _ in 0..RATE_LIMIT_MAX_ATTEMPTS {
            let err = auth_login(State(state.clone()), Json(bad_login("Wr0ngPass1")))
                .await
                .unwrap_err();
            assert_eq!(err.status(), StatusCode::UNAUTHORIZED);
        }
        // Sixth attempt is rejected before any password check.
        let err = auth_login(State(state.clone()), Json(bad_login("Wr0ngPass1")))
            .await
            .unwrap_err();
        assert_eq!(err.status(), StatusCode::TOO_MANY_REQUESTS);
        // Even the correct password is locked out inside the window.
        let err = auth_login(State(state.clone()), Json(bad_login("Str0ngPass1")))
            .await
            .unwrap_err();
        assert_eq!(err.status(), StatusCode::TOO_MANY_REQUESTS);
    }

    #[tokio::test]
    async fn successful_login_resets_rate_limit() {
        let db = test_pool().await;
        let state = test_state(db);
        bootstrap_alice(&state).await;

        let attempt = |password: &str| {
            let state = state.clone();
            let password = password.to_string();
            async move {
                auth_login(
                    State(state),
                    Json(LoginRequest {
                        username: "alice".into(),
                        password,
                    }),
                )
                .await
            }
        };
        // Four failures stay under the cap; a success clears the counter.
        for _ in 0..4 {
            assert!(attempt("Wr0ngPass1").await.is_err());
        }
        assert!(attempt("Str0ngPass1").await.is_ok());
        // Four more failures must not trip the limit either.
        for _ in 0..4 {
            let err = attempt("Wr0ngPass1").await.unwrap_err();
            assert_eq!(err.status(), StatusCode::UNAUTHORIZED);
        }
    }

    #[tokio::test]
    async fn totp_verify_rate_limited_after_five_failures() {
        let db = test_pool().await;
        seed_user_with_totp(&db, "user-1", "alice", "Str0ngPass1").await;
        let state = test_state(db);

        let login = auth_login(
            State(state.clone()),
            Json(LoginRequest {
                username: "alice".into(),
                password: "Str0ngPass1".into(),
            }),
        )
        .await
        .unwrap();
        assert!(login.requires_totp);
        let pending = login.token.clone();

        for _ in 0..RATE_LIMIT_MAX_ATTEMPTS {
            let err = totp_verify(
                State(state.clone()),
                Json(TotpVerifyRequest {
                    code: "000000".into(),
                    pending_token: pending.clone(),
                }),
            )
            .await
            .unwrap_err();
            assert_eq!(err.status(), StatusCode::UNAUTHORIZED);
        }
        let err = totp_verify(
            State(state.clone()),
            Json(TotpVerifyRequest {
                code: "000000".into(),
                pending_token: pending,
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(err.status(), StatusCode::TOO_MANY_REQUESTS);
    }

    #[tokio::test]
    async fn change_password_kicks_all_sessions() {
        let db = test_pool().await;
        let state = test_state(db);
        let token_a = bootstrap_alice(&state).await;

        let login_b = auth_login(
            State(state.clone()),
            Json(LoginRequest {
                username: "alice".into(),
                password: "Str0ngPass1".into(),
            }),
        )
        .await
        .unwrap();
        let token_b = login_b.token.clone();

        let alice = AuthUser(state.sessions.get_session(&token_a).await.unwrap());
        let status = change_password(
            State(state.clone()),
            alice,
            Json(ChangePasswordRequest {
                current_password: "Str0ngPass1".into(),
                new_password: "N3wPassword!x".into(),
            }),
        )
        .await
        .unwrap();
        assert_eq!(status, StatusCode::NO_CONTENT);

        // Every prior session (including the caller's) is dead.
        assert!(state.sessions.get_session(&token_a).await.is_none());
        assert!(state.sessions.get_session(&token_b).await.is_none());

        // Old password fails, new password logs in.
        let old = auth_login(
            State(state.clone()),
            Json(LoginRequest {
                username: "alice".into(),
                password: "Str0ngPass1".into(),
            }),
        )
        .await;
        assert!(old.is_err());
        let new = auth_login(
            State(state.clone()),
            Json(LoginRequest {
                username: "alice".into(),
                password: "N3wPassword!x".into(),
            }),
        )
        .await;
        assert!(new.is_ok());
    }

    #[tokio::test]
    async fn change_password_wrong_current_password_rejected() {
        let db = test_pool().await;
        let state = test_state(db);
        let token = bootstrap_alice(&state).await;

        let alice = AuthUser(state.sessions.get_session(&token).await.unwrap());
        let err = change_password(
            State(state.clone()),
            alice,
            Json(ChangePasswordRequest {
                current_password: "Wr0ngPass1".into(),
                new_password: "N3wPassword!x".into(),
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(err.status(), StatusCode::UNAUTHORIZED);
        // The session is untouched and the old password still works.
        assert!(state.sessions.get_session(&token).await.is_some());
        assert!(
            auth_login(
                State(state.clone()),
                Json(LoginRequest {
                    username: "alice".into(),
                    password: "Str0ngPass1".into(),
                }),
            )
            .await
            .is_ok()
        );
    }

    #[tokio::test]
    async fn change_password_weak_new_password_rejected() {
        let db = test_pool().await;
        let state = test_state(db);
        let token = bootstrap_alice(&state).await;

        let alice = AuthUser(state.sessions.get_session(&token).await.unwrap());
        let err = change_password(
            State(state.clone()),
            alice,
            Json(ChangePasswordRequest {
                current_password: "Str0ngPass1".into(),
                new_password: "weak".into(),
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(err.status(), StatusCode::BAD_REQUEST);
        assert!(state.sessions.get_session(&token).await.is_some());
    }

    #[tokio::test]
    async fn totp_disable_requires_current_password() {
        let db = test_pool().await;
        seed_user_with_totp(&db, "user-1", "alice", "Str0ngPass1").await;
        let state = test_state(db);
        let token = state.sessions.create_session("user-1").await.unwrap();

        let alice = AuthUser(state.sessions.get_session(&token).await.unwrap());
        // Wrong password → rejected, TOTP stays enabled.
        let err = totp_disable(
            State(state.clone()),
            AuthUser(alice.0.clone()),
            Json(TotpDisableRequest {
                password: "Wr0ngPass1".into(),
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(err.status(), StatusCode::UNAUTHORIZED);
        let enabled: bool =
            sqlx::query_scalar("SELECT totp_enabled FROM lyra_user WHERE id = 'user-1'")
                .fetch_one(sqlite_pool(state.db()))
                .await
                .unwrap();
        assert!(enabled);

        // Correct password → disabled.
        let status = totp_disable(
            State(state.clone()),
            alice,
            Json(TotpDisableRequest {
                password: "Str0ngPass1".into(),
            }),
        )
        .await
        .unwrap();
        assert!(!status.totp_enabled);
    }

    #[tokio::test]
    async fn totp_enroll_rejected_when_already_enabled() {
        let db = test_pool().await;
        seed_user_with_totp(&db, "user-1", "alice", "Str0ngPass1").await;
        let state = test_state(db);
        let token = state.sessions.create_session("user-1").await.unwrap();

        let err = totp_enroll(
            State(state.clone()),
            AuthUser(state.sessions.get_session(&token).await.unwrap()),
        )
        .await
        .unwrap_err();
        assert_eq!(err.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn totp_code_cannot_be_replayed() {
        let db = test_pool().await;
        let secret = seed_user_with_totp(&db, "user-1", "alice", "Str0ngPass1").await;
        let state = test_state(db);
        let totp = build_totp(&secret, "alice").unwrap();
        let code = totp.generate_current().unwrap();

        let login = || LoginRequest {
            username: "alice".into(),
            password: "Str0ngPass1".into(),
        };
        let first = auth_login(State(state.clone()), Json(login()))
            .await
            .unwrap();
        let res = totp_verify(
            State(state.clone()),
            Json(TotpVerifyRequest {
                code: code.clone(),
                pending_token: first.token.clone(),
            }),
        )
        .await
        .unwrap();
        assert!(!res.requires_totp);

        // Fresh login + same code → rejected as a replay.
        let second = auth_login(State(state.clone()), Json(login()))
            .await
            .unwrap();
        let err = totp_verify(
            State(state.clone()),
            Json(TotpVerifyRequest {
                code,
                pending_token: second.token.clone(),
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(err.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn totp_rate_limit_survives_fresh_pending_token() {
        let db = test_pool().await;
        seed_user_with_totp(&db, "user-1", "alice", "Str0ngPass1").await;
        let state = test_state(db);

        let login = || LoginRequest {
            username: "alice".into(),
            password: "Str0ngPass1".into(),
        };
        // Burn the 5 attempts on pending token A.
        let first = auth_login(State(state.clone()), Json(login()))
            .await
            .unwrap();
        for _ in 0..RATE_LIMIT_MAX_ATTEMPTS {
            let err = totp_verify(
                State(state.clone()),
                Json(TotpVerifyRequest {
                    code: "000000".into(),
                    pending_token: first.token.clone(),
                }),
            )
            .await
            .unwrap_err();
            assert_eq!(err.status(), StatusCode::UNAUTHORIZED);
        }

        // Regression: a fresh login mints pending token B, but the limiter is
        // keyed per user, so the bypass attempt is still blocked.
        let second = auth_login(State(state.clone()), Json(login()))
            .await
            .unwrap();
        assert_ne!(first.token, second.token);
        let err = totp_verify(
            State(state.clone()),
            Json(TotpVerifyRequest {
                code: "000000".into(),
                pending_token: second.token.clone(),
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(err.status(), StatusCode::TOO_MANY_REQUESTS);
    }

    #[tokio::test]
    async fn change_password_rate_limited_after_five_wrong_current() {
        let db = test_pool().await;
        let state = test_state(db);
        let token = bootstrap_alice(&state).await;

        let user_id = state.sessions.get_session(&token).await.unwrap();
        let attempt = |current: &str| {
            change_password(
                State(state.clone()),
                AuthUser(user_id.clone()),
                Json(ChangePasswordRequest {
                    current_password: current.into(),
                    new_password: "N3wPassword!x".into(),
                }),
            )
        };
        for _ in 0..RATE_LIMIT_MAX_ATTEMPTS {
            let err = attempt("Wr0ngPass1").await.unwrap_err();
            assert_eq!(err.status(), StatusCode::UNAUTHORIZED);
        }
        // Even the correct current password is locked out inside the window.
        let err = attempt("Str0ngPass1").await.unwrap_err();
        assert_eq!(err.status(), StatusCode::TOO_MANY_REQUESTS);
    }

    #[tokio::test]
    async fn totp_disable_rate_limited_after_five_wrong_passwords() {
        let db = test_pool().await;
        seed_user_with_totp(&db, "user-1", "alice", "Str0ngPass1").await;
        let state = test_state(db);
        let token = state.sessions.create_session("user-1").await.unwrap();

        let user_id = state.sessions.get_session(&token).await.unwrap();
        let attempt = |password: &str| {
            totp_disable(
                State(state.clone()),
                AuthUser(user_id.clone()),
                Json(TotpDisableRequest {
                    password: password.into(),
                }),
            )
        };
        for _ in 0..RATE_LIMIT_MAX_ATTEMPTS {
            let err = attempt("Wr0ngPass1").await.unwrap_err();
            assert_eq!(err.status(), StatusCode::UNAUTHORIZED);
        }
        let err = attempt("Str0ngPass1").await.unwrap_err();
        assert_eq!(err.status(), StatusCode::TOO_MANY_REQUESTS);

        // TOTP must still be enabled after the blocked attempts.
        let enabled: bool =
            sqlx::query_scalar("SELECT totp_enabled FROM lyra_user WHERE id = 'user-1'")
                .fetch_one(sqlite_pool(state.db()))
                .await
                .unwrap();
        assert!(enabled);
    }

    #[tokio::test]
    async fn change_password_clears_login_rate_limit() {
        let db = test_pool().await;
        let state = test_state(db);
        let token = bootstrap_alice(&state).await;

        // An attacker locks out the username at the login endpoint.
        for _ in 0..RATE_LIMIT_MAX_ATTEMPTS {
            let err = auth_login(
                State(state.clone()),
                Json(LoginRequest {
                    username: "alice".into(),
                    password: "Wr0ngPass1".into(),
                }),
            )
            .await
            .unwrap_err();
            assert_eq!(err.status(), StatusCode::UNAUTHORIZED);
        }

        // The legit user (valid session) changes their password...
        change_password(
            State(state.clone()),
            AuthUser(state.sessions.get_session(&token).await.unwrap()),
            Json(ChangePasswordRequest {
                current_password: "Str0ngPass1".into(),
                new_password: "N3wPassword!x".into(),
            }),
        )
        .await
        .unwrap();

        // ...and can immediately log in with it: the attacker's login lockout
        // was cleared by the password change.
        let res = auth_login(
            State(state.clone()),
            Json(LoginRequest {
                username: "alice".into(),
                password: "N3wPassword!x".into(),
            }),
        )
        .await;
        assert!(res.is_ok());
    }

    #[tokio::test]
    async fn auth_user_extractor_resolves_bearer_token() {
        let db = test_pool().await;
        seed_user(&db, "user-1").await;
        let state = test_state(db);
        let token = state.sessions.create_session("user-1").await.unwrap();

        let req = axum::http::Request::builder()
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .body(())
            .unwrap();
        let (mut parts, ()) = req.into_parts();
        let user = AuthUser::from_request_parts(&mut parts, &state)
            .await
            .expect("valid token must resolve");
        assert_eq!(user.0, "user-1");
    }

    #[tokio::test]
    async fn auth_user_extractor_rejects_missing_or_invalid_token() {
        let db = test_pool().await;
        let state = test_state(db);

        // No Authorization header.
        let req = axum::http::Request::builder().body(()).unwrap();
        let (mut parts, ()) = req.into_parts();
        let err = AuthUser::from_request_parts(&mut parts, &state)
            .await
            .err()
            .expect("missing header must fail");
        assert_eq!(err.status(), StatusCode::UNAUTHORIZED);

        // Unknown token.
        let req = axum::http::Request::builder()
            .header(header::AUTHORIZATION, "Bearer deadbeef")
            .body(())
            .unwrap();
        let (mut parts, ()) = req.into_parts();
        let err = AuthUser::from_request_parts(&mut parts, &state)
            .await
            .err()
            .expect("invalid token must fail");
        assert_eq!(err.status(), StatusCode::UNAUTHORIZED);
    }
}
