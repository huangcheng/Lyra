//! `/api/v1/settings/ai` + `/api/v1/ai/draft` — thin handlers over the
//! `ai` module. GET never returns the key (only `hasKey`).

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::ai::settings::AiFeatures;
use crate::ai::{
    AiDialect, AiSettings, draft_reply, load_settings, save_settings, test_connection,
};
use crate::api_error::ApiErrorBody;
use crate::auth::{AuthState, AuthUser};

impl IntoResponse for crate::ai::AiError {
    fn into_response(self) -> Response {
        use crate::ai::AiError;
        let (status, message, code) = match &self {
            AiError::NotConfigured => (
                StatusCode::CONFLICT,
                self.to_string(),
                Some("ai_not_configured"),
            ),
            AiError::FeatureDisabled => (
                StatusCode::FORBIDDEN,
                self.to_string(),
                Some("ai_feature_disabled"),
            ),
            AiError::InvalidInput(_) => (
                StatusCode::BAD_REQUEST,
                self.to_string(),
                Some("bad_request"),
            ),
            AiError::Settings(_) => (
                StatusCode::BAD_REQUEST,
                self.to_string(),
                Some("bad_request"),
            ),
            AiError::Timeout | AiError::Unreachable(_) | AiError::Provider(_) => (
                StatusCode::BAD_GATEWAY,
                self.to_string(),
                Some("bad_gateway"),
            ),
            // DB/crypto detail stays server-side.
            masked @ (AiError::Db(_) | AiError::Crypto(_)) => {
                tracing::error!(error = %masked, "AI request failed");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal error".to_string(),
                    Some("internal_error"),
                )
            }
        };
        (status, Json(ApiErrorBody::new(message, code))).into_response()
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AiSettingsResponse {
    enabled: bool,
    dialect: AiDialect,
    base_url: String,
    model: String,
    has_key: bool,
    features: AiFeatures,
}

fn response_for(s: &AiSettings) -> Json<AiSettingsResponse> {
    Json(AiSettingsResponse {
        enabled: s.enabled,
        dialect: s.dialect,
        base_url: s.base_url.clone(),
        model: s.model.clone(),
        has_key: s.has_key(),
        features: s.features,
    })
}

async fn get_ai(
    State(state): State<AuthState>,
    AuthUser(user_id): AuthUser,
) -> Json<AiSettingsResponse> {
    response_for(
        &load_settings(state.db(), &user_id)
            .await
            .unwrap_or_else(|_| AiSettings::empty()),
    )
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct PutAiSettingsRequest {
    enabled: Option<bool>,
    dialect: Option<String>,
    base_url: Option<String>,
    model: Option<String>,
    /// Write-only: `None` keeps the stored key, `""` clears it.
    api_key: Option<String>,
    features: Option<AiFeatures>,
}

async fn put_ai(
    State(state): State<AuthState>,
    AuthUser(user_id): AuthUser,
    Json(body): Json<PutAiSettingsRequest>,
) -> Result<Json<AiSettingsResponse>, crate::ai::AiError> {
    let db = state.db();
    let mut current = load_settings(db, &user_id).await?;
    if let Some(enabled) = body.enabled {
        current.enabled = enabled;
    }
    if let Some(dialect) = &body.dialect {
        current.dialect = AiDialect::parse(dialect).ok_or_else(|| {
            crate::ai::AiError::InvalidInput(format!("unknown dialect: {dialect}"))
        })?;
    }
    if let Some(base_url) = body.base_url {
        current.base_url = base_url;
    }
    if let Some(model) = body.model {
        current.model = model;
    }
    if let Some(features) = body.features {
        current.features = features;
    }
    if let Some(api_key) = body.api_key {
        let blob = if api_key.trim().is_empty() {
            String::new()
        } else {
            let dek = crate::auth::AuthState::get_user_dek(db, &user_id).await?;
            let encrypted = crate::crypto::encrypt(&dek, api_key.trim().as_bytes())?;
            serde_json::to_string(&encrypted)
                .map_err(|e| crate::ai::AiError::InvalidInput(format!("key not storable: {e}")))?
        };
        current = current.with_key_blob(blob);
    }
    save_settings(db, &user_id, &current).await?;
    Ok(response_for(&load_settings(db, &user_id).await?))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AiTestResponse {
    ok: bool,
    reply: String,
    model: String,
}

async fn test_ai(
    State(state): State<AuthState>,
    AuthUser(user_id): AuthUser,
) -> Result<Json<AiTestResponse>, crate::ai::AiError> {
    let reply = test_connection(&state, &user_id).await?;
    let settings = load_settings(state.db(), &user_id).await?;
    Ok(Json(AiTestResponse {
        ok: true,
        reply,
        model: settings.model,
    }))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AiDraftRequest {
    message_id: String,
    mode: String,
    instruction: Option<String>,
}

async fn post_draft(
    State(state): State<AuthState>,
    AuthUser(user_id): AuthUser,
    Json(body): Json<AiDraftRequest>,
) -> Result<Json<serde_json::Value>, crate::ai::AiError> {
    if body.mode != "reply" && body.mode != "forward" {
        return Err(crate::ai::AiError::InvalidInput(
            "mode must be reply or forward".into(),
        ));
    }
    let text = draft_reply(
        &state,
        &user_id,
        &body.message_id,
        &body.mode,
        body.instruction.as_deref(),
    )
    .await?;
    Ok(Json(serde_json::json!({ "text": text })))
}

async fn get_chat(
    State(state): State<AuthState>,
    AuthUser(user_id): AuthUser,
) -> Result<Json<Vec<crate::ai::chat::ChatHistoryEntry>>, crate::ai::AiError> {
    Ok(Json(
        crate::ai::chat::load_history(state.db(), &user_id).await?,
    ))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AiChatRequest {
    message: String,
    /// Open message the panel attached as context (optional).
    message_id: Option<String>,
}

async fn post_chat(
    State(state): State<AuthState>,
    AuthUser(user_id): AuthUser,
    Json(body): Json<AiChatRequest>,
) -> Result<Json<serde_json::Value>, crate::ai::AiError> {
    let message = body.message.trim().to_string();
    if message.is_empty() || message.chars().count() > 8000 {
        return Err(crate::ai::AiError::InvalidInput(
            "message must be 1-8000 characters".into(),
        ));
    }
    let reply =
        crate::ai::chat::chat(&state, &user_id, &message, body.message_id.as_deref()).await?;
    Ok(Json(serde_json::json!({ "reply": reply })))
}

async fn delete_chat(
    State(state): State<AuthState>,
    AuthUser(user_id): AuthUser,
) -> Result<Json<serde_json::Value>, crate::ai::AiError> {
    crate::ai::chat::clear(state.db(), &user_id).await?;
    Ok(Json(serde_json::json!({ "cleared": true })))
}

pub fn routes() -> Router<AuthState> {
    Router::new()
        .route("/api/v1/settings/ai", get(get_ai).put(put_ai))
        .route("/api/v1/settings/ai/test", post(test_ai))
        .route("/api/v1/ai/draft", post(post_draft))
        .route(
            "/api/v1/ai/chat",
            get(get_chat).post(post_chat).delete(delete_chat),
        )
}
