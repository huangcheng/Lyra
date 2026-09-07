//! HTTP surface for Web Push: VAPID key, subscription CRUD, mute-pref
//! write-through, and a full-path test push. All routes require the bearer
//! session (`AuthUser`).

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::auth::{AuthState, AuthUser};
use crate::kv::KvStore;

use super::send::{SendOutcome, send_push};
use super::store::{
    StoredKeys, StoredPrefs, StoredSubscription, load_or_generate_vapid, load_subscriptions,
    remove_subscription, save_prefs, upsert_subscription,
};

pub(crate) fn routes() -> Router<AuthState> {
    Router::new()
        .route("/api/v1/push/vapid-key", get(get_vapid_key))
        .route("/api/v1/push/status", get(get_status))
        .route(
            "/api/v1/push/subscription",
            put(put_subscription).delete(delete_subscription),
        )
        .route("/api/v1/push/prefs", put(put_prefs))
        .route("/api/v1/push/test", post(post_test))
}

#[derive(Debug)]
pub(crate) enum PushHttpError {
    BadRequest(String),
    Internal(String),
}

impl IntoResponse for PushHttpError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            PushHttpError::BadRequest(m) => (StatusCode::BAD_REQUEST, m),
            PushHttpError::Internal(m) => (StatusCode::INTERNAL_SERVER_ERROR, m),
        };
        crate::api_error::api_error(status, message)
    }
}

impl From<crate::kv::KvError> for PushHttpError {
    fn from(e: crate::kv::KvError) -> Self {
        PushHttpError::Internal(e.to_string())
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct VapidKeyResponse {
    pub(crate) public_key: String,
}

pub(crate) async fn get_vapid_key(
    State(state): State<AuthState>,
    AuthUser(_user_id): AuthUser,
) -> Result<Json<VapidKeyResponse>, PushHttpError> {
    let identity = load_or_generate_vapid(state.kv()).await?;
    Ok(Json(VapidKeyResponse {
        public_key: identity.public_key_b64,
    }))
}

#[derive(Serialize)]
pub(crate) struct StatusResponse {
    pub(crate) devices: usize,
}

pub(crate) async fn get_status(
    State(state): State<AuthState>,
    AuthUser(user_id): AuthUser,
) -> Result<Json<StatusResponse>, PushHttpError> {
    let subs = load_subscriptions(state.kv(), &user_id).await?;
    Ok(Json(StatusResponse {
        devices: subs.len(),
    }))
}

#[derive(Deserialize)]
pub(crate) struct PutSubscription {
    pub(crate) endpoint: String,
    pub(crate) keys: StoredKeys,
}

fn valid_endpoint(endpoint: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(endpoint) else {
        return false;
    };
    if url.scheme() == "https" {
        return true;
    }
    // Local dev / test listeners.
    url.scheme() == "http" && matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"))
}

pub(crate) async fn put_subscription(
    State(state): State<AuthState>,
    AuthUser(user_id): AuthUser,
    Json(req): Json<PutSubscription>,
) -> Result<StatusCode, PushHttpError> {
    if !valid_endpoint(&req.endpoint) || req.keys.p256dh.is_empty() || req.keys.auth.is_empty() {
        return Err(PushHttpError::BadRequest(
            "endpoint must be an https push URL and keys must be non-empty".into(),
        ));
    }
    let sub = StoredSubscription {
        endpoint: req.endpoint,
        keys: req.keys,
        created_at: chrono::Utc::now().to_rfc3339(),
    };
    upsert_subscription(state.kv(), &user_id, sub).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub(crate) struct DeleteSubscription {
    pub(crate) endpoint: String,
}

pub(crate) async fn delete_subscription(
    State(state): State<AuthState>,
    AuthUser(user_id): AuthUser,
    Json(req): Json<DeleteSubscription>,
) -> Result<StatusCode, PushHttpError> {
    remove_subscription(state.kv(), &user_id, &req.endpoint).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PutPrefs {
    #[serde(default)]
    pub(crate) muted_folder_ids: Vec<String>,
    #[serde(default)]
    pub(crate) muted_thread_ids: Vec<String>,
    #[serde(default = "default_locale")]
    pub(crate) locale: String,
}

fn default_locale() -> String {
    "en".to_string()
}

pub(crate) async fn put_prefs(
    State(state): State<AuthState>,
    AuthUser(user_id): AuthUser,
    Json(req): Json<PutPrefs>,
) -> Result<StatusCode, PushHttpError> {
    let locale = if req.locale == "zh" { "zh" } else { "en" }.to_string();
    save_prefs(
        state.kv(),
        &user_id,
        &StoredPrefs {
            muted_folder_ids: req.muted_folder_ids,
            muted_thread_ids: req.muted_thread_ids,
            locale,
        },
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Serialize)]
pub(crate) struct TestResponse {
    pub(crate) sent: usize,
    pub(crate) removed: usize,
}

pub(crate) async fn post_test(
    State(state): State<AuthState>,
    AuthUser(user_id): AuthUser,
) -> Result<Json<TestResponse>, PushHttpError> {
    let kv: &Arc<dyn KvStore> = state.kv();
    let subs = load_subscriptions(kv, &user_id).await?;
    let prefs = super::store::load_prefs(kv, &user_id).await?;
    let payload = if prefs.locale == "zh" {
        serde_json::json!({ "title": "Lyra 通知已启用", "body": "后台推送工作正常。", "tag": "lyra-test", "data": { "messageId": "" } })
    } else {
        serde_json::json!({ "title": "Lyra notifications are on", "body": "Background push is working.", "tag": "lyra-test", "data": { "messageId": "" } })
    };
    let vapid = load_or_generate_vapid(kv).await?;
    let client = reqwest::Client::new();
    let mut sent = 0;
    let mut removed = 0;
    for sub in &subs {
        match send_push(
            &client,
            &vapid.private_pem,
            &state.vapid_subject,
            sub,
            &payload.to_string(),
        )
        .await
        {
            Ok(SendOutcome::Delivered) => sent += 1,
            Ok(SendOutcome::Gone) => {
                removed += 1;
                let _ = remove_subscription(kv, &user_id, &sub.endpoint).await;
            }
            _ => {}
        }
    }
    Ok(Json(TestResponse { sent, removed }))
}
