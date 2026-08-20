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
///
/// Public auth routes (login, bootstrap, status, TOTP verify) are mounted
/// without authentication. Protected routes (storage, sync, me) require a
/// valid Bearer token via the `require_auth` middleware.
fn app(auth_state: auth::AuthState) -> Router {
    // Public auth routes — no token required
    let public_auth = auth::routes();

    // Protected API routes — require valid Bearer token
    let protected = Router::new()
        .merge(storage::routes())
        .merge(sync::routes())
        .layer(axum::middleware::from_fn_with_state(
            auth_state.clone(),
            auth::require_auth,
        ));

    Router::new()
        .route("/health", get(health))
        .route("/version", get(version))
        .merge(public_auth)
        .merge(protected)
        .layer(CorsLayer::permissive())
        .with_state(auth_state)
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
    let app_state = storage::create_app_state().await?;

    // Create session store
    let sessions = auth::SessionStore::new();

    let auth_state = auth::AuthState {
        db: app_state.db.clone(),
        sessions,
        min_password_length: cfg.min_password_length,
    };

    tracing::info!("Lyra backend starting on {}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app(auth_state)).await?;

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
        let app_state = storage::create_test_state().await;
        let sessions = auth::SessionStore::new();
        let auth_state = auth::AuthState {
            db: app_state.db,
            sessions,
            min_password_length: 8,
        };
        app(auth_state)
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

    #[tokio::test]
    async fn protected_route_rejects_unauthenticated() {
        let app = test_app().await;
        let req = Request::builder()
            .uri("/api/storage/status")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn auth_flow_bootstrap_login_protected_route() {
        use axum::http::header;

        let app = test_app().await;

        // 1. Bootstrap first user
        let req = Request::builder()
            .method("POST")
            .uri("/api/auth/bootstrap")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                serde_json::to_vec(&serde_json::json!({
                    "username": "testuser",
                    "password": "Str0ngP@ss"
                }))
                .unwrap(),
            ))
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let login_resp: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let token = login_resp["token"].as_str().unwrap();

        // 2. Call protected route with token
        let req = Request::builder()
            .uri("/api/storage/status")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        // 3. Call protected route without token → 401
        let req = Request::builder()
            .uri("/api/storage/status")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }
}
