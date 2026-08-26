//! User database queries.

use axum::http::StatusCode;
use sqlx::Row;

use crate::db_row::{id_from_row, id_param};
use crate::storage::DbPool;

use super::types::{AuthError, UserInfo};

pub(crate) struct UserData {
    pub id: String,
    pub username: String,
    pub password_hash: Option<String>,
    pub totp_enabled: bool,
    pub totp_secret: Option<String>,
    pub display_name: Option<String>,
    pub locale: String,
    pub mark_read_policy: String,
}

pub(crate) fn parse_mark_read_policy(raw: &str) -> Result<String, AuthError> {
    match raw {
        "on_open" | "on_scroll_end" | "manual" => Ok(raw.to_string()),
        _ => Err(AuthError::BadRequest(
            "markReadPolicy must be on_open, on_scroll_end, or manual".into(),
        )),
    }
}

pub(crate) fn parse_stored_mark_read_policy(raw: String) -> String {
    if matches!(raw.as_str(), "on_open" | "on_scroll_end" | "manual") {
        raw
    } else {
        "on_open".to_string()
    }
}

pub(crate) fn user_info_from(user: &UserData) -> UserInfo {
    UserInfo {
        id: user.id.clone(),
        username: user.username.clone(),
        display_name: user.display_name.clone(),
        locale: user.locale.clone(),
        totp_enabled: user.totp_enabled,
        mark_read_policy: user.mark_read_policy.clone(),
    }
}

// ── Database operations ─────────────────────────────────────────────

pub(crate) async fn has_any_user(db: &DbPool) -> Result<bool, StatusCode> {
    let count: i64 = match db {
        DbPool::Sqlite(pool) => {
            sqlx::query_scalar("SELECT COUNT(*) FROM lyra_user")
                .fetch_one(pool)
                .await
        }
        #[cfg(feature = "postgres")]
        DbPool::Postgres(pool) => {
            sqlx::query_scalar("SELECT COUNT(*) FROM lyra_user")
                .fetch_one(pool)
                .await
        }
    }
    .map_err(|e| {
        tracing::error!("DB error checking user count: {e}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    Ok(count > 0)
}

pub(crate) async fn find_first_user_totp_enabled(db: &DbPool) -> Result<bool, StatusCode> {
    match db {
        DbPool::Sqlite(pool) => {
            let row = sqlx::query("SELECT totp_enabled FROM lyra_user LIMIT 1")
                .fetch_optional(pool)
                .await
                .map_err(|e| {
                    tracing::error!("DB error: {e}");
                    StatusCode::INTERNAL_SERVER_ERROR
                })?;
            Ok(row.is_some_and(|r| r.get::<bool, _>("totp_enabled")))
        }
        #[cfg(feature = "postgres")]
        DbPool::Postgres(pool) => {
            let row = sqlx::query("SELECT totp_enabled FROM lyra_user LIMIT 1")
                .fetch_optional(pool)
                .await
                .map_err(|e| {
                    tracing::error!("DB error: {e}");
                    StatusCode::INTERNAL_SERVER_ERROR
                })?;
            Ok(row.is_some_and(|r| r.get::<bool, _>("totp_enabled")))
        }
    }
}

pub(crate) async fn insert_user(
    db: &DbPool,
    id: &str,
    username: &str,
    password_hash: &str,
    display_name: Option<&str>,
    locale: &str,
    encrypted_dek: &str,
) -> Result<(), sqlx::Error> {
    let id = id_param(db, id)?;
    match db {
        DbPool::Sqlite(pool) => {
            sqlx::query(
                "INSERT INTO lyra_user (id, username, password_hash, display_name, locale, encrypted_dek) VALUES (?, ?, ?, ?, ?, ?)",
            )
            .bind(&id)
            .bind(username)
            .bind(password_hash)
            .bind(display_name)
            .bind(locale)
            .bind(encrypted_dek)
            .execute(pool)
            .await?;
        }
        #[cfg(feature = "postgres")]
        DbPool::Postgres(pool) => {
            sqlx::query(
                "INSERT INTO lyra_user (id, username, password_hash, display_name, locale, encrypted_dek) VALUES ($1, $2, $3, $4, $5, $6)",
            )
            .bind(&id)
            .bind(username)
            .bind(password_hash)
            .bind(display_name)
            .bind(locale)
            .bind(encrypted_dek)
            .execute(pool)
            .await?;
        }
    }
    Ok(())
}

/// True when `e` is a unique-constraint violation (e.g. the `singleton`
/// guard rejecting a second row, or a duplicate username).
pub(crate) fn is_unique_violation(e: &sqlx::Error) -> bool {
    match e {
        sqlx::Error::Database(db_err) => db_err.is_unique_violation(),
        _ => false,
    }
}

pub(crate) async fn find_user_by_username(
    db: &DbPool,
    username: &str,
) -> Result<Option<UserData>, AuthError> {
    match db {
        DbPool::Sqlite(pool) => {
            let row = sqlx::query(
                "SELECT id, username, password_hash, totp_enabled, totp_secret, display_name, locale, mark_read_policy FROM lyra_user WHERE username = ?",
            )
            .bind(username)
            .fetch_optional(pool)
            .await
            .map_err(|e| {
                tracing::error!("DB error finding user: {e}");
                AuthError::internal("Authentication failed")
            })?;
            Ok(row.map(|r| UserData {
                id: id_from_row(&r, "id"),
                username: r.get("username"),
                password_hash: Some(r.get("password_hash")),
                totp_enabled: r.get("totp_enabled"),
                totp_secret: r.get("totp_secret"),
                display_name: r.get("display_name"),
                locale: r.get("locale"),
                mark_read_policy: parse_stored_mark_read_policy(r.get("mark_read_policy")),
            }))
        }
        #[cfg(feature = "postgres")]
        DbPool::Postgres(pool) => {
            let row = sqlx::query(
                "SELECT id, username, password_hash, totp_enabled, totp_secret, display_name, locale, mark_read_policy FROM lyra_user WHERE username = $1",
            )
            .bind(username)
            .fetch_optional(pool)
            .await
            .map_err(|e| {
                tracing::error!("DB error finding user: {e}");
                AuthError::internal("Authentication failed")
            })?;
            Ok(row.map(|r| UserData {
                id: id_from_row(&r, "id"),
                username: r.get("username"),
                password_hash: Some(r.get("password_hash")),
                totp_enabled: r.get("totp_enabled"),
                totp_secret: r.get("totp_secret"),
                display_name: r.get("display_name"),
                locale: r.get("locale"),
                mark_read_policy: parse_stored_mark_read_policy(r.get("mark_read_policy")),
            }))
        }
    }
}

pub(crate) async fn find_user_by_id(
    db: &DbPool,
    user_id: &str,
) -> Result<Option<UserData>, AuthError> {
    let Ok(id) = id_param(db, user_id) else {
        return Ok(None);
    };
    match db {
        DbPool::Sqlite(pool) => {
            let row = sqlx::query(
                "SELECT id, username, password_hash, totp_enabled, totp_secret, display_name, locale, mark_read_policy FROM lyra_user WHERE id = ?",
            )
            .bind(&id)
            .fetch_optional(pool)
            .await
            .map_err(|e| {
                tracing::error!("DB error finding user by ID: {e}");
                AuthError::internal("Failed to look up user")
            })?;
            Ok(row.map(|r| UserData {
                id: id_from_row(&r, "id"),
                username: r.get("username"),
                password_hash: Some(r.get("password_hash")),
                totp_enabled: r.get("totp_enabled"),
                totp_secret: r.get("totp_secret"),
                display_name: r.get("display_name"),
                locale: r.get("locale"),
                mark_read_policy: parse_stored_mark_read_policy(r.get("mark_read_policy")),
            }))
        }
        #[cfg(feature = "postgres")]
        DbPool::Postgres(pool) => {
            let row = sqlx::query(
                "SELECT id, username, password_hash, totp_enabled, totp_secret, display_name, locale, mark_read_policy FROM lyra_user WHERE id = $1",
            )
            .bind(&id)
            .fetch_optional(pool)
            .await
            .map_err(|e| {
                tracing::error!("DB error finding user by ID: {e}");
                AuthError::internal("Failed to look up user")
            })?;
            Ok(row.map(|r| UserData {
                id: id_from_row(&r, "id"),
                username: r.get("username"),
                password_hash: Some(r.get("password_hash")),
                totp_enabled: r.get("totp_enabled"),
                totp_secret: r.get("totp_secret"),
                display_name: r.get("display_name"),
                locale: r.get("locale"),
                mark_read_policy: parse_stored_mark_read_policy(r.get("mark_read_policy")),
            }))
        }
    }
}

pub(crate) async fn update_user_totp(
    db: &DbPool,
    user_id: &str,
    totp_secret: Option<&str>,
    totp_enabled: bool,
) -> Result<(), StatusCode> {
    let id = id_param(db, user_id).map_err(|_| StatusCode::NOT_FOUND)?;
    match db {
        DbPool::Sqlite(pool) => {
            sqlx::query(
                "UPDATE lyra_user SET totp_secret = ?, totp_enabled = ?, updated_at = datetime('now') WHERE id = ?",
            )
            .bind(totp_secret)
            .bind(totp_enabled)
            .bind(&id)
            .execute(pool)
            .await
            .map_err(|e| {
                tracing::error!("Failed to update TOTP: {e}");
                StatusCode::INTERNAL_SERVER_ERROR
            })?;
        }
        #[cfg(feature = "postgres")]
        DbPool::Postgres(pool) => {
            sqlx::query(
                "UPDATE lyra_user SET totp_secret = $1, totp_enabled = $2, updated_at = NOW() WHERE id = $3",
            )
            .bind(totp_secret)
            .bind(totp_enabled)
            .bind(&id)
            .execute(pool)
            .await
            .map_err(|e| {
                tracing::error!("Failed to update TOTP: {e}");
                StatusCode::INTERNAL_SERVER_ERROR
            })?;
        }
    }
    Ok(())
}

pub(crate) async fn update_user_password(
    db: &DbPool,
    user_id: &str,
    password_hash: &str,
) -> Result<(), StatusCode> {
    let id = id_param(db, user_id).map_err(|_| StatusCode::NOT_FOUND)?;
    match db {
        DbPool::Sqlite(pool) => {
            sqlx::query(
                "UPDATE lyra_user SET password_hash = ?, updated_at = datetime('now') WHERE id = ?",
            )
            .bind(password_hash)
            .bind(&id)
            .execute(pool)
            .await
            .map_err(|e| {
                tracing::error!("Failed to update password: {e}");
                StatusCode::INTERNAL_SERVER_ERROR
            })?;
        }
        #[cfg(feature = "postgres")]
        DbPool::Postgres(pool) => {
            sqlx::query(
                "UPDATE lyra_user SET password_hash = $1, updated_at = NOW() WHERE id = $2",
            )
            .bind(password_hash)
            .bind(&id)
            .execute(pool)
            .await
            .map_err(|e| {
                tracing::error!("Failed to update password: {e}");
                StatusCode::INTERNAL_SERVER_ERROR
            })?;
        }
    }
    Ok(())
}
