//! Authentication module stub.
//!
//! v1: Username/password + optional TOTP.
//! The handler layer stays thin; real logic lives behind a trait.

use axum::{Json, Router, routing::get};
use serde::Serialize;

/// Routes for auth-related endpoints.
pub fn routes() -> Router {
    Router::new().route("/api/auth/status", get(auth_status))
}

#[derive(Serialize)]
pub struct AuthStatus {
    pub authenticated: bool,
}

/// Stub: always returns unauthenticated until the auth module is implemented.
async fn auth_status() -> Json<AuthStatus> {
    Json(AuthStatus {
        authenticated: false,
    })
}
