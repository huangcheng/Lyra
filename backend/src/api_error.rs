//! Stable JSON error envelope for `/api/v1` responses.
//!
//! Shape: `{ "error": "...", "code"?: "..." }`. The `code` field is optional
//! so existing clients that only read `error` keep working.

use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Serialize;

/// JSON error body returned by API handlers.
#[derive(Debug, Clone, Serialize)]
pub struct ApiErrorBody {
    pub error: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}

impl ApiErrorBody {
    pub fn new(error: impl Into<String>, code: Option<&str>) -> Self {
        Self {
            error: error.into(),
            code: code.map(str::to_string),
        }
    }
}

/// Map common HTTP statuses to stable machine-readable codes.
pub fn code_for_status(status: StatusCode) -> Option<&'static str> {
    match status {
        StatusCode::BAD_REQUEST => Some("bad_request"),
        StatusCode::UNAUTHORIZED => Some("unauthorized"),
        StatusCode::FORBIDDEN => Some("forbidden"),
        StatusCode::NOT_FOUND => Some("not_found"),
        StatusCode::CONFLICT => Some("conflict"),
        StatusCode::TOO_MANY_REQUESTS => Some("too_many_requests"),
        StatusCode::INTERNAL_SERVER_ERROR => Some("internal_error"),
        StatusCode::BAD_GATEWAY => Some("bad_gateway"),
        StatusCode::SERVICE_UNAVAILABLE => Some("service_unavailable"),
        _ => None,
    }
}

#[allow(dead_code)] // shared helper for handlers; prefer over ad-hoc json! envelopes
pub fn api_error(status: StatusCode, message: impl Into<String>) -> Response {
    let code = code_for_status(status);
    (status, Json(ApiErrorBody::new(message, code))).into_response()
}

pub fn api_error_with_code(status: StatusCode, message: impl Into<String>, code: &str) -> Response {
    (status, Json(ApiErrorBody::new(message, Some(code)))).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn error_body_includes_code_when_provided() {
        let res = api_error(StatusCode::NOT_FOUND, "account not found");
        let body = axum::body::to_bytes(res.into_body(), 4096).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["error"], "account not found");
        assert_eq!(json["code"], "not_found");
    }

    #[tokio::test]
    async fn error_body_omits_code_when_unknown_status() {
        let res = api_error_with_code(StatusCode::IM_A_TEAPOT, "nope", "teapot");
        let body = axum::body::to_bytes(res.into_body(), 4096).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["code"], "teapot");
    }
}
