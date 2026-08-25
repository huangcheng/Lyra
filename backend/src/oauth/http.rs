//! HTTP: Microsoft mail OAuth start + callback.

use axum::{
    Json, Router,
    extract::{Query, State},
    http::StatusCode,
    response::{IntoResponse, Redirect, Response},
    routing::get,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::microsoft::{
    MsOAuthError, build_authorize_url, exchange_code, generate_pkce, generate_state,
};
use super::tokens::{OAuthTokenSet, encrypt_oauth_tokens};
use crate::auth::{AuthState, AuthUser};
use crate::db_row::id_param;
use crate::storage::DbPool;

const OUTLOOK_IMAP_HOST: &str = "outlook.office365.com";
const OUTLOOK_SMTP_HOST: &str = "smtp.office365.com";
const PENDING_TTL_SECS: u64 = 600;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct StatusResponse {
    configured: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct StartResponse {
    authorize_url: String,
}

#[derive(Debug, Deserialize)]
struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct PendingOAuth {
    user_id: String,
    pkce_verifier: String,
}

impl IntoResponse for MsOAuthError {
    fn into_response(self) -> Response {
        let status = match &self {
            MsOAuthError::NotConfigured => StatusCode::SERVICE_UNAVAILABLE,
            MsOAuthError::InvalidState | MsOAuthError::MissingEmail => StatusCode::BAD_REQUEST,
            MsOAuthError::TokenExchange(_) => StatusCode::BAD_GATEWAY,
            MsOAuthError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        let msg = match &self {
            MsOAuthError::Internal(e) => {
                tracing::error!(error = %e, "oauth internal error");
                "internal error".to_string()
            }
            MsOAuthError::TokenExchange(e) => {
                tracing::warn!(error = %e, "oauth token exchange failed");
                "token exchange failed".to_string()
            }
            other => other.to_string(),
        };
        (status, Json(serde_json::json!({ "error": msg }))).into_response()
    }
}

pub fn routes() -> Router<AuthState> {
    Router::new()
        .route("/api/v1/oauth/microsoft/status", get(oauth_status))
        .route("/api/v1/oauth/microsoft/start", get(oauth_start))
        .route(
            "/api/v1/oauth/microsoft/callback",
            get(oauth_callback),
        )
}

async fn oauth_status(State(state): State<AuthState>) -> Json<StatusResponse> {
    Json(StatusResponse {
        configured: state.ms_oauth.is_some(),
    })
}

async fn oauth_start(
    State(state): State<AuthState>,
    AuthUser(user_id): AuthUser,
) -> Result<Json<StartResponse>, MsOAuthError> {
    let cfg = state.ms_oauth.as_ref().ok_or(MsOAuthError::NotConfigured)?;
    let state_token = generate_state();
    let pkce = generate_pkce();
    let pending = PendingOAuth {
        user_id,
        pkce_verifier: pkce.verifier.clone(),
    };
    let raw = serde_json::to_string(&pending)
        .map_err(|e| MsOAuthError::Internal(e.to_string()))?;
    state
        .kv()
        .set(
            &pending_key(&state_token),
            &raw,
            Some(PENDING_TTL_SECS),
        )
        .await
        .map_err(|e| MsOAuthError::Internal(e.to_string()))?;

    let authorize_url = build_authorize_url(cfg, &state_token, &pkce);
    Ok(Json(StartResponse { authorize_url }))
}

async fn oauth_callback(
    State(state): State<AuthState>,
    Query(q): Query<CallbackQuery>,
) -> Result<Response, MsOAuthError> {
    if let Some(err) = q.error {
        let desc = q.error_description.unwrap_or_default();
        tracing::warn!(error = %err, desc = %desc, "microsoft oauth denied");
        return Ok(redirect_settings("error", "oauth_denied"));
    }
    let code = q.code.ok_or(MsOAuthError::InvalidState)?;
    let state_token = q.state.ok_or(MsOAuthError::InvalidState)?;
    let cfg = state.ms_oauth.as_ref().ok_or(MsOAuthError::NotConfigured)?;

    let pending_raw = state
        .kv()
        .get(&pending_key(&state_token))
        .await
        .map_err(|e| MsOAuthError::Internal(e.to_string()))?
        .ok_or(MsOAuthError::InvalidState)?;
    let _ = state.kv().del(&pending_key(&state_token)).await;
    let pending: PendingOAuth = serde_json::from_str(&pending_raw)
        .map_err(|_| MsOAuthError::InvalidState)?;

    let exchanged = exchange_code(cfg, &code, &pending.pkce_verifier).await?;
    let email = exchanged
        .email
        .filter(|e| e.contains('@'))
        .ok_or(MsOAuthError::MissingEmail)?
        .to_lowercase();

    let dek = crate::auth::AuthState::get_user_dek(&state.db, &pending.user_id)
        .await
        .map_err(|e| MsOAuthError::Internal(e.to_string()))?;

    let tokens = OAuthTokenSet {
        access_token: exchanged.access_token,
        refresh_token: exchanged
            .refresh_token
            .ok_or_else(|| MsOAuthError::TokenExchange("missing refresh".into()))?,
        expires_at: exchanged.expires_at,
        token_type: "Bearer".into(),
        scope: exchanged.scope.unwrap_or_default(),
    };
    let credential = encrypt_oauth_tokens(&dek, &tokens)?;

    let account_id = Uuid::new_v7(uuid::Timestamp::now(uuid::NoContext)).to_string();
    let user_bind = id_param(&state.db, &pending.user_id)
        .map_err(|_| MsOAuthError::Internal("user id".into()))?;
    let id_bind = id_param(&state.db, &account_id)
        .map_err(|_| MsOAuthError::Internal("account id".into()))?;

    // Upsert-by-email for this user: replace prior Outlook oauth row if present.
    if let Some(existing) = find_oauth_account(&state.db, &pending.user_id, &email).await? {
        db_execute!(
            &state.db,
            r"
            UPDATE mail_account SET
                credential = ?, auth_type = 'oauth2',
                imap_host = ?, imap_port = ?, imap_security = 'tls',
                smtp_host = ?, smtp_port = ?, smtp_security = 'starttls',
                protocol = 'imap', receive_protocol = 'imap', send_protocol = 'smtp',
                is_active = 1, sync_enabled = 1,
                updated_at = datetime('now')
            WHERE id = ?
            ",
            &credential,
            OUTLOOK_IMAP_HOST,
            993_i32,
            OUTLOOK_SMTP_HOST,
            587_i32,
            &id_param(&state.db, &existing)
                .map_err(|_| MsOAuthError::Internal("existing id".into()))?
        )
        .map_err(|e| MsOAuthError::Internal(e.to_string()))?;
        return Ok(redirect_settings("ok", "reconnected"));
    }

    let display = email.split('@').next().unwrap_or("Outlook").to_string();
    db_execute!(
        &state.db,
        r"
        INSERT INTO mail_account (
            id, user_id, display_name, email_address, protocol, auth_type,
            credential, imap_host, imap_port, imap_security,
            smtp_host, smtp_port, smtp_security,
            receive_protocol, send_protocol,
            is_active, sync_enabled
        ) VALUES (?, ?, ?, ?, 'imap', 'oauth2', ?, ?, ?, 'tls', ?, ?, 'starttls', 'imap', 'smtp', 1, 1)
        ",
        &id_bind,
        &user_bind,
        &display,
        &email,
        &credential,
        OUTLOOK_IMAP_HOST,
        993_i32,
        OUTLOOK_SMTP_HOST,
        587_i32
    )
    .map_err(|e| MsOAuthError::Internal(e.to_string()))?;

    Ok(redirect_settings("ok", "connected"))
}

fn pending_key(state: &str) -> String {
    format!("oauth:ms:pending:{state}")
}

fn redirect_settings(status: &str, detail: &str) -> Response {
    let loc = format!("/settings?section=accounts&oauth={status}&detail={detail}");
    Redirect::temporary(&loc).into_response()
}

async fn find_oauth_account(
    db: &DbPool,
    user_id: &str,
    email: &str,
) -> Result<Option<String>, MsOAuthError> {
    let user_bind =
        id_param(db, user_id).map_err(|_| MsOAuthError::Internal("user id".into()))?;
    let row = db_fetch_optional!(
        db,
        r"
        SELECT id FROM mail_account
        WHERE user_id = ? AND lower(email_address) = lower(?) AND auth_type = 'oauth2'
        ",
        |row| crate::db_row::id_from_row(&row, "id"),
        &user_bind,
        email
    )
    .map_err(|e| MsOAuthError::Internal(e.to_string()))?;
    Ok(row)
}
