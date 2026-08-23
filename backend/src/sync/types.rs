//! Sync API types and errors.

use axum::{http::StatusCode, Json, response::IntoResponse};
use serde::Serialize;

use crate::db_row::InvalidIdError;
use crate::imap::ImapError;
use crate::jmap::JmapError;
use crate::smtp::SmtpError;

// ── API types ───────────────────────────────────────────────────────

/// Response for sync status endpoint.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncStatus {
    pub active_accounts: i64,
    pub syncing: bool,
}

/// Response for a sync operation.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncResponse {
    pub account_id: String,
    pub status: String,
    pub folders_synced: usize,
    pub messages_synced: usize,
    pub messages_updated: usize,
    pub messages_deleted: usize,
}

/// HTTP 202 body when a sync job is enqueued.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnqueuedSync {
    pub job_id: String,
    pub status: String,
}

/// Error type for sync operations.
#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error("account not found")]
    AccountNotFound,
    #[error("message not found")]
    MessageNotFound,
    #[error("account not active or sync disabled")]
    AccountDisabled,
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("imap error: {0}")]
    Imap(#[from] ImapError),
    #[error("jmap error: {0}")]
    Jmap(#[from] JmapError),
    #[error("smtp error: {0}")]
    Smtp(#[from] SmtpError),
    #[error("crypto error: {0}")]
    Crypto(String),
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("protocol error: {0}")]
    Protocol(String),
}

impl From<InvalidIdError> for SyncError {
    fn from(_: InvalidIdError) -> Self {
        Self::InvalidInput("invalid id".into())
    }
}

impl IntoResponse for SyncError {
    fn into_response(self) -> axum::response::Response {
        // 5xx/upstream variants carry hostnames, SQL detail, and protocol
        // chatter: log the full error server-side, answer "internal error".
        // 4xx variants are deliberate API surface and stay descriptive.
        let (status, message) = match &self {
            SyncError::AccountNotFound | SyncError::MessageNotFound => {
                (StatusCode::NOT_FOUND, self.to_string())
            }
            SyncError::AccountDisabled | SyncError::InvalidInput(_) => {
                (StatusCode::BAD_REQUEST, self.to_string())
            }
            // Crypto messages carry deliberate operator guidance, no secrets.
            SyncError::Crypto(_) => (StatusCode::INTERNAL_SERVER_ERROR, self.to_string()),
            masked @ (SyncError::Database(_)
            | SyncError::Imap(_)
            | SyncError::Jmap(_)
            | SyncError::Smtp(_)
            | SyncError::Protocol(_)) => {
                tracing::error!(error = %masked, "sync request failed");
                let status = if matches!(masked, SyncError::Database(_)) {
                    StatusCode::INTERNAL_SERVER_ERROR
                } else {
                    StatusCode::BAD_GATEWAY
                };
                (status, "internal error".to_string())
            }
        };
        (status, Json(serde_json::json!({ "error": message }))).into_response()
    }
}
