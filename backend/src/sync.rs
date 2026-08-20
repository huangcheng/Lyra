//! Sync engine module stub.
//!
//! The sync engine is Lyra's deepest module. It orchestrates `JMAP` / `IMAP`
//! adapters, writes to storage, and emits events for the UI.
//! See `docs/specs/2026-08-20-lyra-sync-and-protocols-spec.md`.

use axum::{Json, Router, extract::State, routing::get};
use serde::Serialize;

use crate::storage::AppState;

/// Routes for sync-related endpoints.
pub fn routes() -> Router<AppState> {
    Router::new().route("/api/sync/status", get(sync_status))
}

#[derive(Serialize)]
pub struct SyncStatus {
    pub active_accounts: u32,
    pub syncing: bool,
}

/// Stub: reports sync status.
async fn sync_status(State(_state): State<AppState>) -> Json<SyncStatus> {
    Json(SyncStatus {
        active_accounts: 0,
        syncing: false,
    })
}
