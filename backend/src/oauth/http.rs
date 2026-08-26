//! HTTP: mail-account OAuth start + shared callback.

use axum::{
    Json, Router,
    extract::{Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Redirect, Response},
    routing::get,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::microsoft::{
    MsOAuthError, build_authorize_url as build_microsoft_authorize_url,
    exchange_code as exchange_microsoft_code, generate_pkce, generate_state,
};
use super::providers::{self, MICROSOFT, MailServerDefaults, YANDEX};
use super::tokens::{OAuthTokenSet, encrypt_oauth_tokens};
use super::yandex::{
    build_authorize_url as build_yandex_authorize_url, exchange_code as exchange_yandex_code,
};
use crate::api_error::ApiErrorBody;
use crate::auth::{AuthState, AuthUser};
use crate::db_row::id_param;
use crate::storage::DbPool;

const PENDING_TTL_SECS: u64 = 600;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProvidersResponse {
    providers: Vec<providers::ProviderInfo>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct StartResponse {
    authorize_url: String,
    provider: String,
}

#[derive(Debug, Deserialize)]
struct StartQuery {
    email: String,
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
    provider: String,
    pkce_verifier: String,
    email_hint: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct OAuthCallbackResponse {
    status: String,
    detail: String,
}

impl IntoResponse for MsOAuthError {
    fn into_response(self) -> Response {
        let status = match &self {
            MsOAuthError::NotConfigured | MsOAuthError::ProviderNotConfigured(_) => {
                StatusCode::SERVICE_UNAVAILABLE
            }
            MsOAuthError::InvalidState
            | MsOAuthError::MissingEmail
            | MsOAuthError::MissingEmailParam
            | MsOAuthError::UnknownProvider(_) => StatusCode::BAD_REQUEST,
            MsOAuthError::TokenExchange(_) => StatusCode::BAD_GATEWAY,
            MsOAuthError::CredentialDecrypt | MsOAuthError::Internal(_) => {
                StatusCode::INTERNAL_SERVER_ERROR
            }
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
        let code = match &self {
            MsOAuthError::NotConfigured | MsOAuthError::ProviderNotConfigured(_) => {
                Some("service_unavailable")
            }
            MsOAuthError::InvalidState
            | MsOAuthError::MissingEmail
            | MsOAuthError::MissingEmailParam => Some("bad_request"),
            MsOAuthError::UnknownProvider(_) => Some("bad_request"),
            MsOAuthError::TokenExchange(_) => Some("bad_gateway"),
            MsOAuthError::CredentialDecrypt | MsOAuthError::Internal(_) => Some("internal_error"),
        };
        (status, Json(ApiErrorBody::new(msg, code))).into_response()
    }
}

pub fn routes() -> Router<AuthState> {
    Router::new()
        .route("/api/v1/oauth/providers", get(oauth_providers))
        .route("/api/v1/oauth/start", get(oauth_start))
        .route("/api/v1/oauth/callback", get(oauth_callback))
}

async fn oauth_providers(
    State(state): State<AuthState>,
    AuthUser(_user_id): AuthUser,
) -> Json<ProvidersResponse> {
    Json(ProvidersResponse {
        providers: providers::list_providers(&state),
    })
}

async fn oauth_start(
    State(state): State<AuthState>,
    AuthUser(user_id): AuthUser,
    Query(q): Query<StartQuery>,
) -> Result<Json<StartResponse>, MsOAuthError> {
    let email = q.email.trim();
    if !email.contains('@') {
        return Err(MsOAuthError::MissingEmailParam);
    }
    let provider = providers::resolve_provider(email).ok_or_else(|| {
        MsOAuthError::UnknownProvider(
            email
                .split('@')
                .nth(1)
                .unwrap_or(email)
                .to_ascii_lowercase(),
        )
    })?;
    if !providers::is_configured(&state, provider) {
        return Err(MsOAuthError::ProviderNotConfigured(provider.into()));
    }

    let state_token = generate_state();
    let pkce = generate_pkce();
    let email_hint = email.to_ascii_lowercase();

    let pending = PendingOAuth {
        user_id,
        provider: provider.into(),
        pkce_verifier: pkce.verifier.clone(),
        email_hint: Some(email_hint.clone()),
    };
    let raw = serde_json::to_string(&pending).map_err(|e| MsOAuthError::Internal(e.to_string()))?;
    state
        .kv()
        .set(&pending_key(&state_token), &raw, Some(PENDING_TTL_SECS))
        .await
        .map_err(|e| MsOAuthError::Internal(e.to_string()))?;

    let authorize_url = match provider {
        MICROSOFT => {
            let cfg = state.ms_oauth.as_ref().ok_or(MsOAuthError::NotConfigured)?;
            build_microsoft_authorize_url(cfg, &state_token, &pkce)
        }
        YANDEX => {
            let cfg = state
                .yandex_oauth
                .as_ref()
                .ok_or(MsOAuthError::NotConfigured)?;
            build_yandex_authorize_url(cfg, &state_token, &pkce, Some(&email_hint))
        }
        _ => return Err(MsOAuthError::UnknownProvider(provider.into())),
    };

    Ok(Json(StartResponse {
        authorize_url,
        provider: provider.into(),
    }))
}

async fn oauth_callback(
    State(state): State<AuthState>,
    headers: HeaderMap,
    Query(q): Query<CallbackQuery>,
) -> Result<Response, MsOAuthError> {
    if let Some(err) = q.error {
        let desc = q.error_description.unwrap_or_default();
        tracing::warn!(error = %err, desc = %desc, "mail oauth denied");
        return Ok(oauth_complete_response(&headers, "error", "oauth_denied"));
    }
    let code = q.code.ok_or(MsOAuthError::InvalidState)?;
    let state_token = q.state.ok_or(MsOAuthError::InvalidState)?;

    let pending_raw = state
        .kv()
        .get(&pending_key(&state_token))
        .await
        .map_err(|e| MsOAuthError::Internal(e.to_string()))?
        .ok_or(MsOAuthError::InvalidState)?;
    let _ = state.kv().del(&pending_key(&state_token)).await;
    let pending: PendingOAuth =
        serde_json::from_str(&pending_raw).map_err(|_| MsOAuthError::InvalidState)?;

    let servers = providers::mail_servers(&pending.provider)
        .ok_or_else(|| MsOAuthError::UnknownProvider(pending.provider.clone()))?;

    let exchanged = match pending.provider.as_str() {
        MICROSOFT => {
            let cfg = state.ms_oauth.as_ref().ok_or(MsOAuthError::NotConfigured)?;
            match exchange_microsoft_code(cfg, &code, &pending.pkce_verifier).await {
                Ok(tokens) => tokens,
                Err(MsOAuthError::TokenExchange(e)) => {
                    tracing::warn!(error = %e, "oauth token exchange failed");
                    return Ok(oauth_complete_response(&headers, "error", "token_exchange"));
                }
                Err(e) => return Err(e),
            }
        }
        YANDEX => {
            let cfg = state
                .yandex_oauth
                .as_ref()
                .ok_or(MsOAuthError::NotConfigured)?;
            match exchange_yandex_code(cfg, &code, &pending.pkce_verifier).await {
                Ok(tokens) => tokens,
                Err(MsOAuthError::TokenExchange(e)) => {
                    tracing::warn!(error = %e, "oauth token exchange failed");
                    return Ok(oauth_complete_response(&headers, "error", "token_exchange"));
                }
                Err(e) => return Err(e),
            }
        }
        _ => return Err(MsOAuthError::UnknownProvider(pending.provider.clone())),
    };

    let email = exchanged
        .email
        .or(pending.email_hint)
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

    let reconnected =
        persist_oauth_account(&state.db, &pending.user_id, &email, &credential, &servers).await?;

    let detail = if reconnected {
        "reconnected"
    } else {
        "connected"
    };
    Ok(oauth_complete_response(&headers, "ok", detail))
}

async fn persist_oauth_account(
    db: &DbPool,
    user_id: &str,
    email: &str,
    credential: &str,
    servers: &MailServerDefaults,
) -> Result<bool, MsOAuthError> {
    let user_bind = id_param(db, user_id).map_err(|_| MsOAuthError::Internal("user id".into()))?;

    if let Some(existing) = find_oauth_account(db, user_id, email).await? {
        db_execute!(
            db,
            r"
            UPDATE mail_account SET
                credential = ?, auth_type = 'oauth2',
                imap_host = ?, imap_port = ?, imap_security = 'tls',
                smtp_host = ?, smtp_port = ?, smtp_security = ?,
                protocol = 'imap', receive_protocol = 'imap', send_protocol = 'smtp',
                is_active = 1, sync_enabled = 1,
                updated_at = datetime('now')
            WHERE id = ?
            ",
            credential,
            servers.imap_host,
            servers.imap_port,
            servers.smtp_host,
            servers.smtp_port,
            servers.smtp_security,
            &id_param(db, &existing).map_err(|_| MsOAuthError::Internal("existing id".into()))?
        )
        .map_err(|e| MsOAuthError::Internal(e.to_string()))?;
        return Ok(true);
    }

    let account_id = Uuid::new_v7(uuid::Timestamp::now(uuid::NoContext)).to_string();
    let id_bind =
        id_param(db, &account_id).map_err(|_| MsOAuthError::Internal("account id".into()))?;
    let display = email.split('@').next().unwrap_or("Mail").to_string();

    db_execute!(
        db,
        r"
        INSERT INTO mail_account (
            id, user_id, display_name, email_address, protocol, auth_type,
            credential, imap_host, imap_port, imap_security,
            smtp_host, smtp_port, smtp_security,
            receive_protocol, send_protocol,
            is_active, sync_enabled
        ) VALUES (?, ?, ?, ?, 'imap', 'oauth2', ?, ?, ?, 'tls', ?, ?, ?, 'imap', 'smtp', 1, 1)
        ",
        &id_bind,
        &user_bind,
        &display,
        email,
        credential,
        servers.imap_host,
        servers.imap_port,
        servers.smtp_host,
        servers.smtp_port,
        servers.smtp_security
    )
    .map_err(|e| MsOAuthError::Internal(e.to_string()))?;
    Ok(false)
}

fn wants_json_callback(headers: &HeaderMap) -> bool {
    headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|accept| {
            accept.split(',').any(|part| {
                part.split(';')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .eq_ignore_ascii_case("application/json")
            })
        })
}

fn oauth_complete_response(headers: &HeaderMap, status: &str, detail: &str) -> Response {
    if wants_json_callback(headers) {
        Json(OAuthCallbackResponse {
            status: status.to_string(),
            detail: detail.to_string(),
        })
        .into_response()
    } else {
        redirect_settings(status, detail)
    }
}

fn pending_key(state: &str) -> String {
    format!("oauth:pending:{state}")
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
    let user_bind = id_param(db, user_id).map_err(|_| MsOAuthError::Internal("user id".into()))?;
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
