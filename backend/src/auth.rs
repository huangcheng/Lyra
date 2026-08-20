//! Authentication module stub.
//!
//! v1: Username/password + optional TOTP.
//! The handler layer stays thin; real logic lives behind a trait.

use axum::{Json, Router, extract::State, routing::get};
use serde::Serialize;

use crate::storage::AppState;

/// Routes for auth-related endpoints.
pub fn routes() -> Router<AppState> {
    Router::new().route("/api/auth/status", get(auth_status))
}

#[derive(Serialize)]
pub struct AuthStatus {
    pub authenticated: bool,
}

/// Stub: always returns unauthenticated until the auth module is implemented.
async fn auth_status(State(_state): State<AppState>) -> Json<AuthStatus> {
    Json(AuthStatus {
        authenticated: false,
    })
}
