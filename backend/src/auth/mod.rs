//! Authentication module.
//!
//! v1: Username/password + optional TOTP.
//! Single-user instance; the handler layer stays thin; real logic lives behind functions.

use std::sync::Arc;

use axum::{
    Router,
    extract::FromRequestParts,
    http::{HeaderMap, header, request::Parts},
    routing::{get, patch, post},
};

mod db;
mod dek;
mod handlers;
mod password;
mod session;
mod state;
#[cfg(test)]
mod tests;
mod totp;
mod types;

#[cfg(test)]
pub(crate) use dek::{TEST_MASTER_KEY, install_test_master_key};
#[allow(unused_imports)]
pub use session::{SessionStore, invalidate_user_sessions};
pub use state::AuthState;
pub use types::*;

use db::find_user_by_id;
use password::verify_password;
use session::{clear_failed_attempts, ensure_not_rate_limited, note_failed_attempt, pwd_rl_key};

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

pub(crate) fn generate_token() -> String {
    use rand::RngCore;
    let mut rng = rand::thread_rng();
    let mut bytes = [0u8; 32];
    rng.fill_bytes(&mut bytes);
    base32::encode(base32::Alphabet::Rfc4648 { padding: false }, &bytes).to_lowercase()
}

/// Re-authenticate with the account password (rate-limited). Used by
/// password-gated actions outside auth routes (e.g. OpenGPG secret export).
pub async fn verify_current_password(
    state: &AuthState,
    user_id: &str,
    password: &str,
) -> Result<(), AuthError> {
    let kv = Arc::clone(state.sessions.kv());
    let rl_key = pwd_rl_key(user_id);
    ensure_not_rate_limited(kv.as_ref(), &rl_key, "Verification failed").await?;

    let user = find_user_by_id(&state.db, user_id)
        .await?
        .ok_or_else(|| AuthError::internal("User not found"))?;

    let valid = verify_password(
        password,
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
    Ok(())
}

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

/// Authenticated user plus raw bearer token (for session-scoped caches).
pub struct AuthSession {
    pub user_id: String,
    pub token: String,
}

impl FromRequestParts<AuthState> for AuthSession {
    type Rejection = AuthError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AuthState,
    ) -> Result<Self, Self::Rejection> {
        let token = extract_token_from_headers(&parts.headers)
            .ok_or_else(|| AuthError::unauthorized("Missing authorization header"))?;
        let user_id = state
            .sessions
            .get_session(&token)
            .await
            .ok_or_else(|| AuthError::unauthorized("Invalid or expired session"))?;
        Ok(Self { user_id, token })
    }
}

pub(super) fn extract_token_from_headers(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(String::from)
}

pub(super) async fn extract_session(
    state: &AuthState,
    headers: &HeaderMap,
) -> Result<String, AuthError> {
    let token = extract_token_from_headers(headers)
        .ok_or_else(|| AuthError::unauthorized("Missing authorization header"))?;

    state
        .sessions
        .get_session(&token)
        .await
        .ok_or_else(|| AuthError::unauthorized("Invalid or expired session"))
}

pub fn routes() -> Router<AuthState> {
    Router::new()
        .route("/api/v1/auth/status", get(handlers::auth_status))
        .route("/api/v1/auth/bootstrap", post(handlers::auth_bootstrap))
        .route("/api/v1/auth/login", post(handlers::auth_login))
        .route("/api/v1/auth/logout", post(handlers::auth_logout))
        .route("/api/v1/auth/me", get(handlers::auth_me))
        .route(
            "/api/v1/auth/preferences",
            patch(handlers::patch_preferences),
        )
        .route(
            "/api/v1/auth/change-password",
            post(handlers::change_password),
        )
        .route("/api/v1/auth/totp/enroll", post(handlers::totp_enroll))
        .route(
            "/api/v1/auth/totp/confirm",
            post(handlers::totp_enroll_confirm),
        )
        .route("/api/v1/auth/totp/verify", post(handlers::totp_verify))
        .route("/api/v1/auth/totp/disable", post(handlers::totp_disable))
}
