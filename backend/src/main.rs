//! Lyra backend entry point.
//!
//! Self-hosted mail client (not a mail server).
//! Client-agnostic `/api/v1`; web UI is a peer client.
//! See `docs/product/2026-08-20-lyra-v1-product-spec.md`.

#![allow(clippy::doc_markdown)]
#![allow(clippy::unnecessary_wraps)]
#![allow(clippy::match_same_arms)]

mod accounts;
mod auth;
mod config;
mod crypto;
mod dav;
mod imap;
mod jmap;
mod jobs;
mod kernel;
mod pim;
mod plugins;
mod protocol;
mod scheduler;
mod smtp;
mod storage;
mod sync;

use axum::{Json, Router, http::StatusCode};
use std::path::PathBuf;
use tower_http::services::{ServeDir, ServeFile};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let config = config::Config::from_env();
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
    let auth_state = auth::AuthState::new(db, &config, app)?;
    jobs::spawn_workers(
        auth_state.db.clone(),
        std::sync::Arc::clone(&auth_state.app),
        config.sync_max_concurrent,
    );
    scheduler::start_scheduler(auth_state.db.clone(), config.sync_poll_secs);

    let api = Router::new()
        .route("/health", axum::routing::get(health))
        .route("/version", axum::routing::get(version))
        .merge(accounts::routes())
        .merge(pim::routes())
        .merge(sync::routes())
        .merge(auth::routes())
        .with_state(auth_state);

    let frontend_dir = std::env::var("FRONTEND_DIR").unwrap_or_else(|_| "frontend/dist".into());
    let app = if PathBuf::from(&frontend_dir).is_dir() {
        let index = PathBuf::from(&frontend_dir).join("index.html");
        api.fallback_service(ServeDir::new(&frontend_dir).not_found_service(ServeFile::new(index)))
    } else {
        tracing::warn!("FRONTEND_DIR {frontend_dir} missing; API-only mode");
        api
    };

    let listener = tokio::net::TcpListener::bind(&config.listen_addr).await?;
    tracing::info!("listening on {}", config.listen_addr);
    axum::serve(listener, app).await?;

    Ok(())
}

async fn health() -> StatusCode {
    StatusCode::OK
}

async fn version() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
    }))
}
