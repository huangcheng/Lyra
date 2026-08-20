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
mod pim;
mod storage;
mod sync;

use axum::{Json, Router, http::StatusCode};
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
    let auth_state = auth::AuthState::new(db, &config)?;

    let app = Router::new()
        .route("/health", axum::routing::get(health))
        .route("/version", axum::routing::get(version))
        .merge(accounts::routes())
        .merge(pim::routes())
        .merge(sync::routes())
        .merge(auth::routes())
        .with_state(auth_state);

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
