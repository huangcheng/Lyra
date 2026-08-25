//! Lyra backend entry point.
//!
//! Self-hosted mail client (not a mail server).
//! Client-agnostic `/api/v1`; web UI is a peer client.
//! See `docs/product/2026-08-20-lyra-v1-product-spec.md`.

#![allow(clippy::doc_markdown)]
#![allow(clippy::unnecessary_wraps)]
#![allow(clippy::match_same_arms)]

#[macro_use]
mod db_sql;
mod accounts;
mod auth;
mod config;
mod crypto;
mod dav;
mod db_row;
mod imap;
mod imap_idle;
mod jmap;
mod jobs;
mod kernel;
mod kv;
mod media;
mod netsec;
mod pim;
mod plugins;
mod privacy;
mod protocol;
mod sanitize;
mod scheduler;
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
    use axum::body::Body;
    use tower::ServiceExt as _;

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

    async fn test_api_router() -> Router {
        let storage = storage::Storage::new("sqlite::memory:").await.unwrap();
        storage.run_migrations().await.unwrap();
        let config = config::Config {
            listen_addr: "127.0.0.1:0".into(),
            database_url: "sqlite::memory:".into(),
            data_dir: std::env::temp_dir().to_string_lossy().into_owned(),
            min_password_length: 8,
            sync_max_concurrent: 3,
            sync_poll_secs: 300,
            redis_url: None,
            master_key: vec![0x11; 32],
        };
        let state = auth::AuthState::new(
            storage.pool().clone(),
            &config,
            std::sync::Arc::new(kernel::App::new()),
            std::sync::Arc::new(kv::MemoryKv::new()),
        )
        .unwrap();
        api_router(state)
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
}
