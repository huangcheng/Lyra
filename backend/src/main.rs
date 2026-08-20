use axum::{Json, Router, routing::get};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use tower_http::cors::CorsLayer;
use tracing_subscriber::EnvFilter;

mod auth;
mod config;
mod storage;
mod sync;

/// Build the Axum application with all routes.
fn app(state: storage::AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/version", get(version))
        .merge(auth::routes())
        .merge(storage::routes())
        .merge(sync::routes())
        .layer(CorsLayer::permissive())
        .with_state(state)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialise tracing from RUST_LOG env (default: info).
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let cfg = config::Config::from_env();
    let addr: SocketAddr = cfg.listen_addr.parse()?;

    // Create storage and run migrations
    tracing::info!("Initializing database...");
    let state = storage::create_app_state().await?;

    tracing::info!("Lyra backend starting on {}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app(state)).await?;

    Ok(())
}

// ── Health & version routes ─────────────────────────────────────────

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse { status: "ok" })
}

#[derive(Serialize, Deserialize)]
struct VersionResponse {
    version: String,
    name: String,
}

async fn version() -> Json<VersionResponse> {
    Json(VersionResponse {
        version: env!("CARGO_PKG_VERSION").to_string(),
        name: env!("CARGO_PKG_NAME").to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    /// Create a test app with in-memory `SQLite`.
    async fn test_app() -> Router {
        let state = storage::create_test_state().await;
        app(state)
    }

    #[tokio::test]
    async fn health_returns_ok() {
        let app = test_app().await;
        let req = Request::builder()
            .uri("/health")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn version_returns_package_info() {
        let app = test_app().await;
        let req = Request::builder()
            .uri("/version")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let v: VersionResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(v.name, "lyra_backend");
    }
}
