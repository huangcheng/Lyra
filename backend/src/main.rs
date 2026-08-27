//! Lyra backend entry point.
//!
//! Self-hosted mail client (not a mail server).
//! Client-agnostic `/api/v1`; web UI is a peer client.
//! See `docs/product/2026-08-20-lyra-v1-product-spec.md`.

#![allow(clippy::doc_markdown)]
#![allow(clippy::unnecessary_wraps)]
#![allow(clippy::match_same_arms)]

#[macro_use]
mod accounts;
mod api_error;
mod auth;
mod blobs;
mod config;
mod crypto;
mod dav;
mod db_row;
mod entities;
mod imap;
mod imap_idle;
mod jmap;
mod jmap_push;
mod jobs;
mod kernel;
mod kv;
mod media;
mod netsec;
mod oauth;
mod opengpg;
mod pim;
mod plugins;
mod privacy;
mod protocol;
mod repository;
mod sanitize;
mod scheduler;
mod search;
mod smtp;
mod stats;
mod storage;
mod sync;

use axum::{
    Json, Router,
    extract::Request,
    http::{HeaderValue, StatusCode, header},
    middleware,
    response::Response,
};
use std::path::PathBuf;
use tower_http::services::{ServeDir, ServeFile};
use tracing_subscriber::EnvFilter;

/// Matches the CSP meta tag in `frontend/index.html`; sent as a real header
/// because `frame-ancestors` is ignored in `<meta>` form.
const SPA_CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob: https: http:; font-src 'self' data:; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'";

/// Baseline security headers for API and static/SPA responses.
async fn security_headers(req: Request, next: middleware::Next) -> Response {
    let mut res = next.run(req).await;
    let headers = res.headers_mut();
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(SPA_CSP),
    );
    res
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let config = config::Config::from_env()?;
    let storage = storage::Storage::new(&config.database_url).await?;
    storage.run_migrations().await?;
    let db = storage.pool().clone();
    plugins::bind_storage(db.clone());
    let mut app = kernel::App::new();
    app.provide("storage");
    for plugin in plugins::builtin_plugins() {
        app.register_plugin(plugin.as_ref())?;
    }
    let app = std::sync::Arc::new(app);
    let kv: std::sync::Arc<dyn kv::KvStore> = if let Some(url) = config.redis_url.as_deref() {
        std::sync::Arc::new(kv::RedisKv::connect(url).await?)
    } else {
        tracing::warn!("REDIS_URL unset; using in-memory kv (sessions die on restart)");
        std::sync::Arc::new(kv::MemoryKv::new())
    };
    let auth_state = auth::AuthState::new(db, &config, app, kv)?;
    jobs::spawn_workers(
        auth_state.db.clone(),
        std::sync::Arc::clone(&auth_state.app),
        config.sync_max_concurrent,
    );
    scheduler::start_scheduler(auth_state.db.clone(), config.sync_poll_secs);
    imap_idle::start_idle_supervisor(auth_state.db.clone());
    jmap_push::start_jmap_push_supervisor(auth_state.db.clone());

    let api = api_router(auth_state);

    let frontend_dir = std::env::var("FRONTEND_DIR").unwrap_or_else(|_| "frontend/dist".into());
    let app = if PathBuf::from(&frontend_dir).is_dir() {
        let index = PathBuf::from(&frontend_dir).join("index.html");
        api.fallback_service(ServeDir::new(&frontend_dir).not_found_service(ServeFile::new(index)))
    } else {
        tracing::warn!("FRONTEND_DIR {frontend_dir} missing; API-only mode");
        api
    };
    let app = app.layer(middleware::from_fn(security_headers));

    let listener = tokio::net::TcpListener::bind(&config.listen_addr).await?;
    tracing::info!("listening on {}", config.listen_addr);
    axum::serve(listener, app).await?;

    Ok(())
}

/// Public HTTP surface: unversioned `/health` and `/version`, everything else under `/api/v1`.
fn api_router(auth_state: auth::AuthState) -> Router {
    Router::new()
        .route("/health", axum::routing::get(health))
        .route("/version", axum::routing::get(version))
        .merge(accounts::routes())
        .merge(pim::routes())
        .merge(sync::routes())
        .merge(stats::routes())
        .merge(privacy::routes())
        .merge(media::routes())
        .merge(opengpg::routes())
        .merge(oauth::routes())
        .merge(auth::routes())
        .with_state(auth_state)
}

async fn health() -> StatusCode {
    StatusCode::OK
}

async fn version() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{Body, to_bytes};
    use axum::http::Method;
    use storage::DbPool;
    use tower::ServiceExt as _;
    use uuid::Uuid;

    async fn response_json(res: axum::response::Response) -> (StatusCode, serde_json::Value) {
        let status = res.status();
        let bytes = to_bytes(res.into_body(), usize::MAX).await.unwrap();
        let json = if bytes.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
        };
        (status, json)
    }

    async fn request_json(
        app: Router,
        method: Method,
        uri: &str,
        body: Option<&serde_json::Value>,
        token: Option<&str>,
    ) -> (StatusCode, serde_json::Value) {
        let mut builder = Request::builder().method(method).uri(uri);
        if let Some(token) = token {
            builder = builder.header("authorization", format!("Bearer {token}"));
        }
        let req = if let Some(body) = body {
            builder
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap()
        } else {
            builder.body(Body::empty()).unwrap()
        };
        let res = app.oneshot(req).await.unwrap();
        response_json(res).await
    }

    #[tokio::test]
    async fn security_headers_are_applied() {
        let app = Router::new()
            .route("/health", axum::routing::get(health))
            .layer(middleware::from_fn(security_headers));
        let res = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let h = res.headers();
        assert_eq!(h[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
        assert_eq!(h[header::X_FRAME_OPTIONS], "DENY");
        assert_eq!(h[header::REFERRER_POLICY], "no-referrer");
        let csp = h[header::CONTENT_SECURITY_POLICY].to_str().unwrap();
        assert!(csp.contains("frame-ancestors 'none'"));
        assert!(csp.contains("default-src 'self'"));
    }

    async fn test_app() -> (Router, DbPool) {
        auth::install_test_master_key();
        let storage = storage::Storage::new("sqlite::memory:").await.unwrap();
        storage.run_migrations().await.unwrap();
        let db = storage.pool().clone();
        let config = config::Config {
            listen_addr: "127.0.0.1:0".into(),
            database_url: "sqlite::memory:".into(),
            data_dir: std::env::temp_dir().to_string_lossy().into_owned(),
            min_password_length: 8,
            sync_max_concurrent: 3,
            sync_poll_secs: 300,
            redis_url: None,
            master_key: auth::TEST_MASTER_KEY.to_vec(),
            ms_oauth: None,
            yandex_oauth: None,
        };
        let state = auth::AuthState::new(
            db.clone(),
            &config,
            std::sync::Arc::new(kernel::App::new()),
            std::sync::Arc::new(kv::MemoryKv::new()),
        )
        .unwrap();
        (api_router(state), db)
    }

    async fn test_api_router() -> Router {
        test_app().await.0
    }

    async fn bootstrap_user(app: Router) -> (String, String) {
        let (status, json) = request_json(
            app,
            Method::POST,
            "/api/v1/auth/bootstrap",
            Some(&serde_json::json!({
                "username": "alice",
                "password": "Str0ngPass1"
            })),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let user_id = json["user"]["id"].as_str().expect("user id").to_string();
        let token = json["token"].as_str().expect("token").to_string();
        (user_id, token)
    }

    fn sqlite_pool(db: &DbPool) -> &sqlx::SqlitePool {
        match db {
            DbPool::Sqlite(pool) => pool,
            #[cfg(feature = "postgres")]
            DbPool::Postgres(_) => panic!("expected sqlite in tests"),
        }
    }

    async fn seed_account_folder_message(db: &DbPool, user_id: &str) -> (String, String, String) {
        let account_id = Uuid::new_v7(uuid::Timestamp::now(uuid::NoContext)).to_string();
        let folder_id = Uuid::new_v7(uuid::Timestamp::now(uuid::NoContext)).to_string();
        let message_id = Uuid::new_v7(uuid::Timestamp::now(uuid::NoContext)).to_string();
        let pool = sqlite_pool(db);

        let dek = auth::AuthState::get_user_dek(db, user_id).await.unwrap();
        let encrypted = crypto::encrypt(&dek, b"password123").unwrap();
        let credential_json = serde_json::to_string(&encrypted).unwrap();

        sqlx::query(
            r"
            INSERT INTO mail_account (
                id, user_id, display_name, email_address, protocol, auth_type,
                credential, imap_host, imap_port, imap_security,
                is_active, sync_enabled
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 1, 1)
            ",
        )
        .bind(&account_id)
        .bind(user_id)
        .bind("Test Account")
        .bind("alice@example.com")
        .bind("imap")
        .bind("password")
        .bind(&credential_json)
        .bind("imap.example.com")
        .bind(993)
        .bind("tls")
        .execute(pool)
        .await
        .unwrap();

        sqlx::query(
            r"
            INSERT INTO folder (
                id, account_id, external_id, name, role, sort_order
            ) VALUES (?, ?, ?, ?, ?, 0)
            ",
        )
        .bind(&folder_id)
        .bind(&account_id)
        .bind("INBOX")
        .bind("Inbox")
        .bind("inbox")
        .execute(pool)
        .await
        .unwrap();

        sqlx::query(
            r"
            INSERT INTO message (
                id, account_id, folder_id, external_id, subject,
                from_address, date, snippet, is_read
            ) VALUES (?, ?, ?, ?, ?, ?, datetime('now'), ?, 0)
            ",
        )
        .bind(&message_id)
        .bind(&account_id)
        .bind(&folder_id)
        .bind("msg-1")
        .bind("Hello Lyra")
        .bind(r#"{"name":"Bob","email":"bob@example.com"}"#)
        .bind("Snippet text")
        .execute(pool)
        .await
        .unwrap();

        (account_id, folder_id, message_id)
    }

    #[tokio::test]
    async fn api_v1_auth_status_is_mounted() {
        let app = test_api_router().await;
        let res = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/auth/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn unversioned_api_auth_status_is_not_mounted() {
        // API-only mode (no SPA fallback): old `/api/...` paths are a clean 404.
        let app = test_api_router().await;
        let res = app
            .oneshot(
                Request::builder()
                    .uri("/api/auth/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn sync_events_requires_auth() {
        let app = test_api_router().await;
        let res = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/sync/events")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn messages_stats_is_mounted_and_requires_auth() {
        let app = test_api_router().await;
        let res = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/messages/stats?days=7")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        // 401 (not 404): route is mounted behind the same auth extractor.
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn http_login_returns_session_token() {
        let app = test_api_router().await;
        let (_, bootstrap_token) = bootstrap_user(app.clone()).await;

        let (logout_status, _) = request_json(
            app.clone(),
            Method::POST,
            "/api/v1/auth/logout",
            None,
            Some(&bootstrap_token),
        )
        .await;
        assert!(logout_status.is_success());

        let (status, json) = request_json(
            app,
            Method::POST,
            "/api/v1/auth/login",
            Some(&serde_json::json!({
                "username": "alice",
                "password": "Str0ngPass1"
            })),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(json["token"].as_str().is_some());
        let requires_totp = json["requires_totp"]
            .as_bool()
            .or_else(|| json["requiresTotp"].as_bool());
        assert_eq!(requires_totp, Some(false));
    }

    #[tokio::test]
    async fn http_list_folder_messages_returns_seeded_message() {
        let (app, db) = test_app().await;
        let (user_id, token) = bootstrap_user(app.clone()).await;
        let (_, folder_id, message_id) = seed_account_folder_message(&db, &user_id).await;

        let (status, json) = request_json(
            app,
            Method::GET,
            &format!("/api/v1/folders/{folder_id}/messages"),
            None,
            Some(&token),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let messages = json.as_array().expect("message array");
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["id"].as_str(), Some(message_id.as_str()));
        assert_eq!(messages[0]["subject"].as_str(), Some("Hello Lyra"));
    }

    #[tokio::test]
    async fn http_list_opengpg_keys_returns_empty_array() {
        let app = test_api_router().await;
        let (_, token) = bootstrap_user(app.clone()).await;

        let (status, json) =
            request_json(app, Method::GET, "/api/v1/opengpg/keys", None, Some(&token)).await;
        assert_eq!(status, StatusCode::OK);
        let keys = json.as_array().expect("key array");
        assert!(keys.is_empty());
    }
}
