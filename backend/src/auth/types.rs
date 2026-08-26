//! Auth request/response types and errors.

use axum::Json;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde::{Deserialize, Serialize};

use crate::api_error::ApiErrorBody;
use crate::crypto::CryptoError;

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
    pub locale: Option<String>,
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
    pub(crate) fn internal(op: &str) -> Self {
        Self::Internal(op.to_string())
    }

    pub(crate) fn unauthorized(msg: &str) -> Self {
        Self::Unauthorized(msg.to_string())
    }

    pub(crate) fn invalid_credentials() -> Self {
        Self::Unauthorized("Invalid username or password".to_string())
    }

    pub(crate) fn status(&self) -> StatusCode {
        match self {
            AuthError::BadRequest(_) => StatusCode::BAD_REQUEST,
            AuthError::Unauthorized(_) => StatusCode::UNAUTHORIZED,
            AuthError::TooManyRequests => StatusCode::TOO_MANY_REQUESTS,
            AuthError::Conflict(_) => StatusCode::CONFLICT,
            AuthError::Internal(_) | AuthError::Crypto(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    fn error_code(&self) -> Option<&'static str> {
        match self {
            AuthError::BadRequest(_) => Some("bad_request"),
            AuthError::Unauthorized(_) => Some("unauthorized"),
            AuthError::TooManyRequests => Some("too_many_requests"),
            AuthError::Conflict(_) => Some("conflict"),
            AuthError::Internal(_) | AuthError::Crypto(_) => Some("internal_error"),
        }
    }
}

impl IntoResponse for AuthError {
    fn into_response(self) -> axum::response::Response {
        let status = self.status();
        (
            status,
            Json(ApiErrorBody::new(self.to_string(), self.error_code())),
        )
            .into_response()
    }
}

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
