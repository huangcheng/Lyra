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

#[derive(Serialize)]
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
) -> Result<(), StatusCode> {
    match db {
        DbPool::Sqlite(pool) => {
            sqlx::query(
                "INSERT INTO lyra_user (id, username, password_hash, display_name, locale) VALUES (?, ?, ?, ?, ?)",
            )
            .bind(id)
            .bind(username)
            .bind(password_hash)
            .bind(display_name)
            .bind(locale)
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
                "INSERT INTO lyra_user (id, username, password_hash, display_name, locale) VALUES ($1, $2, $3, $4, $5)",
            )
            .bind(id)
            .bind(username)
            .bind(password_hash)
            .bind(display_name)
            .bind(locale)
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

// ── Session store (in-memory for v1) ────────────────────────────────

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Clone)]
pub struct SessionStore {
    sessions: Arc<RwLock<HashMap<String, String>>>,
    pending_sessions: Arc<RwLock<HashMap<String, String>>>,
}

impl SessionStore {
    pub fn new() -> Self {
        Self {
            sessions: Arc::new(RwLock::new(HashMap::new())),
            pending_sessions: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn create_session(&self, user_id: &str) -> String {
        let token = generate_token();
        self.sessions
            .write()
            .await
            .insert(token.clone(), user_id.to_string());
        token
    }

    pub async fn create_pending_session(&self, user_id: &str) -> String {
        let token = generate_token();
        self.pending_sessions
            .write()
            .await
            .insert(token.clone(), user_id.to_string());
        token
    }

    pub async fn get_session(&self, token: &str) -> Option<String> {
        self.sessions.read().await.get(token).cloned()
    }

    pub async fn get_pending_session(&self, token: &str) -> Option<String> {
        self.pending_sessions.read().await.get(token).cloned()
    }

    pub async fn promote_pending_session(&self, pending_token: &str) -> Option<String> {
        let mut pending = self.pending_sessions.write().await;
        if let Some(user_id) = pending.remove(pending_token) {
            drop(pending);
            Some(self.create_session(&user_id).await)
        } else {
            None
        }
    }

    pub async fn remove_session(&self, token: &str) {
        self.sessions.write().await.remove(token);
    }
}

// ── Application state ───────────────────────────────────────────────

#[derive(Clone)]
pub struct AuthState {
    pub db: DbPool,
    pub sessions: SessionStore,
    pub min_password_length: usize,
}

impl AuthState {
    pub fn new(db: DbPool, config: &crate::config::Config) -> Result<Self, anyhow::Error> {
        Ok(Self {
            db,
            sessions: SessionStore::new(),
            min_password_length: config.min_password_length,
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
    /// The DEK is encrypted with the master key and stored in the user record.
    pub fn get_user_dek() -> Result<Vec<u8>, crate::crypto::CryptoError> {
        // For v1, use a default key derived from a constant.
        // In production, this should use the user's encrypted_dek from the database.
        // TODO: Implement proper DEK derivation from user's encrypted_dek
        let master_key = std::env::var("LYRA_MASTER_KEY")
            .unwrap_or_else(|_| "lyra-default-master-key-for-dev-only".to_string());
        let key_bytes = master_key.as_bytes();
        let mut key = [0u8; 32];
        for (i, &b) in key_bytes.iter().enumerate().take(32) {
            key[i] = b;
        }
        Ok(key.to_vec())
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

    insert_user(
        &state.db,
        &user_id,
        &req.username,
        &password_hash,
        req.display_name.as_deref(),
        &locale,
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

    let token = state.sessions.create_session(&user_id).await;

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
        let pending_token = state.sessions.create_pending_session(&user.id).await;
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

    let token = state.sessions.create_session(&user.id).await;

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

    let totp = build_totp(
        user.totp_secret.as_deref().ok_or_else(|| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: "TOTP not configured".to_string(),
                }),
            )
        })?,
        &user.username,
    )
    .map_err(|_| {
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
        .ok_or_else(|| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: "Failed to promote session".to_string(),
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
    update_user_totp(&state.db, &user_id, Some(&secret_base32), false)
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

    let secret = user.totp_secret.ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "TOTP enrollment not started".to_string(),
            }),
        )
    })?;

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

    update_user_totp(&state.db, &user_id, Some(&secret), true)
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
        let store = SessionStore::new();
        let token = store.create_session("user-1").await;
        assert_eq!(store.get_session(&token).await, Some("user-1".to_string()));
        store.remove_session(&token).await;
        assert_eq!(store.get_session(&token).await, None);
    }

    #[tokio::test]
    async fn pending_session_promotion() {
        let store = SessionStore::new();
        let pending_token = store.create_pending_session("user-1").await;
        assert!(store.get_pending_session(&pending_token).await.is_some());
        let session_token = store.promote_pending_session(&pending_token).await;
        assert!(session_token.is_some());
        assert_eq!(
            store.get_session(&session_token.unwrap()).await,
            Some("user-1".to_string())
        );
        assert!(store.get_pending_session(&pending_token).await.is_none());
    }
}
