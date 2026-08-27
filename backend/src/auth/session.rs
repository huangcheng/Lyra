//! Session storage, rate limiting, and invalidation.

use std::sync::Arc;

use axum::http::StatusCode;

use sea_orm::sea_query::Expr;
use sea_orm::{ColumnTrait, DerivePartialModel, EntityTrait, ExprTrait, QueryFilter};

use crate::entities::lyra_user;
use crate::kv::KvStore;
use crate::storage::DbPool;

use super::db::id_bind_value;
use super::types::AuthError;
use super::{PENDING_TTL_SECS, RATE_LIMIT_MAX_ATTEMPTS, RATE_LIMIT_WINDOW_SECS, SESSION_TTL_SECS};

pub(crate) fn sess_key(epoch: i64, token: &str) -> String {
    format!("sess:{epoch}:{token}")
}

pub(crate) fn tok_key(token: &str) -> String {
    format!("tok:{token}")
}

pub(crate) fn pending_key(token: &str) -> String {
    format!("pending:{token}")
}

// ── Rate limiting (fixed window per key, via kv counters) ────────────

// Keyed per username: an attacker who knows the (single, v1) username can
// lock out the legit user for 15 minutes. That lockout-DoS tradeoff is
// deliberate for single-user v1 — the alternative (keying per IP) is
// trivially bypassed behind proxies and we have no trusted client IP yet.
pub(crate) fn login_rl_key(username: &str) -> String {
    format!("rl:login:{username}")
}

/// Keyed per user (not per pending token): re-logging in must not mint a
/// fresh allowance of TOTP attempts.
pub(crate) fn totp_rl_key(user_id: &str) -> String {
    format!("rl:totp:{user_id}")
}

/// Guards password-gated endpoints (change-password, TOTP disable, secret key export)
/// so a stolen session is not an offline-speed password oracle.
pub(crate) fn pwd_rl_key(user_id: &str) -> String {
    format!("rl:pwd:{user_id}")
}

pub(crate) fn totp_step_key(user_id: &str) -> String {
    format!("totp_last_step:{user_id}")
}

/// True once `key` has hit `max_attempts` within its current window.
pub(crate) async fn is_rate_limited(
    kv: &dyn KvStore,
    key: &str,
    max_attempts: i64,
) -> Result<bool, StatusCode> {
    let attempts = kv
        .get(key)
        .await
        .map_err(|e| {
            tracing::error!("rate limit read failed: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(0);
    Ok(attempts >= max_attempts)
}

/// Count a failed attempt. The counter TTL is set on first failure, so the
/// fixed window starts then and restarts after expiry.
pub(crate) async fn record_failed_attempt(
    kv: &dyn KvStore,
    key: &str,
    window_secs: u64,
) -> Result<i64, StatusCode> {
    kv.incr(key, 1, Some(window_secs)).await.map_err(|e| {
        tracing::error!("rate limit incr failed: {e}");
        StatusCode::INTERNAL_SERVER_ERROR
    })
}

/// Reset the counter after a successful attempt.
pub(crate) async fn clear_failed_attempts(kv: &dyn KvStore, key: &str) {
    if let Err(e) = kv.del(key).await {
        tracing::warn!("rate limit clear failed: {e}");
    }
}

/// 429 when `key` is at the attempt cap; kv failures surface as 500 with `op`.
pub(crate) async fn ensure_not_rate_limited(
    kv: &dyn KvStore,
    key: &str,
    op: &str,
) -> Result<(), AuthError> {
    if is_rate_limited(kv, key, RATE_LIMIT_MAX_ATTEMPTS)
        .await
        .map_err(|_| AuthError::internal(op))?
    {
        return Err(AuthError::TooManyRequests);
    }
    Ok(())
}

/// Record a failed attempt; kv failures surface as 500 with `op`.
pub(crate) async fn note_failed_attempt(
    kv: &dyn KvStore,
    key: &str,
    op: &str,
) -> Result<(), AuthError> {
    record_failed_attempt(kv, key, RATE_LIMIT_WINDOW_SECS)
        .await
        .map_err(|_| AuthError::internal(op))?;
    Ok(())
}

/// `sess_epoch` projection — the entity column is `i32` on both dialects,
/// which is exactly what the old per-dialect decode had to emulate.
#[derive(DerivePartialModel)]
#[sea_orm(entity = "lyra_user::Entity")]
struct SessEpochRow {
    sess_epoch: i32,
}

pub(crate) async fn fetch_sess_epoch(db: &DbPool, user_id: &str) -> Result<i64, StatusCode> {
    let id = id_bind_value(db, user_id).map_err(|_| StatusCode::NOT_FOUND)?;
    let row = lyra_user::Entity::find()
        .filter(lyra_user::Column::Id.eq(id))
        .into_partial_model::<SessEpochRow>()
        .one(&db.orm())
        .await
        .map_err(|e| {
            tracing::error!("fetch sess_epoch failed: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .ok_or(StatusCode::NOT_FOUND)?;
    Ok(i64::from(row.sess_epoch))
}

/// Bump `sess_epoch` and delete session keys for the previous epoch.
pub async fn invalidate_user_sessions(
    pool: &DbPool,
    kv: &dyn KvStore,
    user_id: &str,
) -> Result<(), StatusCode> {
    let old_epoch = fetch_sess_epoch(pool, user_id).await?;
    let id = id_bind_value(pool, user_id).map_err(|_| StatusCode::NOT_FOUND)?;
    lyra_user::Entity::update_many()
        .col_expr(
            lyra_user::Column::SessEpoch,
            Expr::col((lyra_user::Entity, lyra_user::Column::SessEpoch)).add(1i32),
        )
        .filter(lyra_user::Column::Id.eq(id))
        .exec(&pool.orm())
        .await
        .map_err(|e| {
            tracing::error!("bump sess_epoch failed: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    kv.del_prefix(&format!("sess:{old_epoch}:"))
        .await
        .map_err(|e| {
            tracing::error!("del_prefix sessions failed: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    Ok(())
}
#[derive(Clone)]
pub struct SessionStore {
    kv: Arc<dyn KvStore>,
    db: DbPool,
}

impl SessionStore {
    pub fn new(db: DbPool, kv: Arc<dyn KvStore>) -> Self {
        Self { kv, db }
    }

    #[must_use]
    pub fn kv(&self) -> &Arc<dyn KvStore> {
        &self.kv
    }

    pub async fn create_session(&self, user_id: &str) -> Result<String, StatusCode> {
        let epoch = fetch_sess_epoch(&self.db, user_id).await?;
        let token = super::generate_token();
        self.kv
            .set(&sess_key(epoch, &token), user_id, Some(SESSION_TTL_SECS))
            .await
            .map_err(|e| {
                tracing::error!("session set failed: {e}");
                StatusCode::INTERNAL_SERVER_ERROR
            })?;
        // Reverse index so get_session can resolve token → user, then check current epoch.
        self.kv
            .set(&tok_key(&token), user_id, Some(SESSION_TTL_SECS))
            .await
            .map_err(|e| {
                tracing::error!("session tok index set failed: {e}");
                StatusCode::INTERNAL_SERVER_ERROR
            })?;
        Ok(token)
    }

    pub async fn create_pending_session(&self, user_id: &str) -> Result<String, StatusCode> {
        let token = super::generate_token();
        self.kv
            .set(&pending_key(&token), user_id, Some(PENDING_TTL_SECS))
            .await
            .map_err(|e| {
                tracing::error!("pending session set failed: {e}");
                StatusCode::INTERNAL_SERVER_ERROR
            })?;
        Ok(token)
    }

    pub async fn get_session(&self, token: &str) -> Option<String> {
        let user_id = self.kv.get(&tok_key(token)).await.ok().flatten()?;
        let epoch = fetch_sess_epoch(&self.db, &user_id).await.ok()?;
        let stored = self.kv.get(&sess_key(epoch, token)).await.ok().flatten()?;
        if stored == user_id {
            Some(user_id)
        } else {
            None
        }
    }

    pub async fn get_pending_session(&self, token: &str) -> Option<String> {
        self.kv.get(&pending_key(token)).await.ok().flatten()
    }

    pub async fn promote_pending_session(
        &self,
        pending_token: &str,
    ) -> Result<Option<String>, StatusCode> {
        let Some(user_id) = self.get_pending_session(pending_token).await else {
            return Ok(None);
        };
        self.kv
            .del(&pending_key(pending_token))
            .await
            .map_err(|e| {
                tracing::error!("pending session del failed: {e}");
                StatusCode::INTERNAL_SERVER_ERROR
            })?;
        Ok(Some(self.create_session(&user_id).await?))
    }

    pub async fn remove_session(&self, token: &str) {
        if let Some(user_id) = self.kv.get(&tok_key(token)).await.ok().flatten()
            && let Ok(epoch) = fetch_sess_epoch(&self.db, &user_id).await
        {
            let _ = self.kv.del(&sess_key(epoch, token)).await;
        }
        let _ = self.kv.del(&tok_key(token)).await;
    }
}
