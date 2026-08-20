//! Storage module stub.
//!
//! Provides the repository traits and (later) `SQLite` / `PostgreSQL` implementations.
//! See `docs/specs/2026-08-20-lyra-data-model-spec.md` for the schema.

use axum::{Json, Router, routing::get};
use serde::Serialize;

/// Routes for storage-related endpoints.
pub fn routes() -> Router {
    Router::new().route("/api/storage/status", get(storage_status))
}

#[derive(Serialize)]
pub struct StorageStatus {
    pub engine: &'static str,
    pub ready: bool,
}

/// Stub: reports storage readiness.
async fn storage_status() -> Json<StorageStatus> {
    Json(StorageStatus {
        engine: "none",
        ready: false,
    })
}
