//! User database queries.

use std::sync::Arc;

use axum::http::StatusCode;
use uuid::Uuid;

use crate::db_row::InvalidIdError;
use crate::entities::lyra_user;
use crate::storage::DbPool;
use sea_orm::sea_query::{Alias, Expr, InsertStatement};
use sea_orm::{
    ColumnTrait, ConnectionTrait, DbErr, DerivePartialModel, EntityTrait, ExprTrait,
    PaginatorTrait, QueryFilter, QuerySelect, Value,
};

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

// ── Entity ↔ legacy row seam ────────────────────────────────────────
//
// Entity PKs are `Uuid`, but the macro layer stores ids as TEXT on SQLite
// (and tests use non-UUID ids there). Ids therefore bind as strings on
// SQLite and as native UUIDs on Postgres — the same split
// `db_row::id_param` makes — and read back through a `CAST(… AS text)`
// projection so both dialects yield the handler-facing `String` id.

/// Bind an id for a UUID-typed entity column.
pub(crate) fn id_bind_value(db: &DbPool, id: &str) -> Result<Value, InvalidIdError> {
    match db {
        DbPool::Sqlite(_) => Ok(Value::String(Some(id.to_owned()))),
        #[cfg(feature = "postgres")]
        DbPool::Postgres(_) => Ok(Value::Uuid(Some(
            Uuid::parse_str(id).map_err(|_| InvalidIdError)?,
        ))),
    }
}

/// Recover the underlying [`sqlx::Error`] from a SeaORM error so the typed
/// sqlx surface (e.g. [`is_unique_violation`]) keeps working.
///
/// The executor wraps each driver error in a fresh `Arc`, so the unwrap
/// normally succeeds and the typed database error survives intact.
pub(crate) fn dberr_to_sqlx(e: DbErr) -> sqlx::Error {
    use sea_orm::RuntimeErr;
    match e {
        DbErr::Exec(RuntimeErr::SqlxError(arc))
        | DbErr::Query(RuntimeErr::SqlxError(arc))
        | DbErr::Conn(RuntimeErr::SqlxError(arc)) => {
            Arc::try_unwrap(arc).unwrap_or_else(|shared| sqlx::Error::Protocol(shared.to_string()))
        }
        other => sqlx::Error::Protocol(other.to_string()),
    }
}

/// `lyra_user` projection with the id read as text.
#[derive(DerivePartialModel)]
#[sea_orm(entity = "lyra_user::Entity")]
struct UserRow {
    #[sea_orm(
        from_expr = "Expr::col((lyra_user::Entity, lyra_user::Column::Id)).cast_as(Alias::new(\"text\"))"
    )]
    id: String,
    username: String,
    password_hash: String,
    totp_secret: Option<String>,
    totp_enabled: bool,
    display_name: Option<String>,
    locale: String,
    mark_read_policy: String,
}

impl From<UserRow> for UserData {
    fn from(row: UserRow) -> Self {
        UserData {
            id: row.id,
            username: row.username,
            password_hash: Some(row.password_hash),
            totp_enabled: row.totp_enabled,
            totp_secret: row.totp_secret,
            display_name: row.display_name,
            locale: row.locale,
            mark_read_policy: parse_stored_mark_read_policy(row.mark_read_policy),
        }
    }
}

// ── Database operations ─────────────────────────────────────────────

pub(crate) async fn has_any_user(db: &DbPool) -> Result<bool, StatusCode> {
    let count = lyra_user::Entity::find()
        .count(&db.orm())
        .await
        .map_err(|e| {
            tracing::error!("DB error checking user count: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    Ok(count > 0)
}

/// First user's TOTP flag — the single-user table makes any row authoritative.
#[derive(DerivePartialModel)]
#[sea_orm(entity = "lyra_user::Entity")]
struct TotpFlagRow {
    totp_enabled: bool,
}

pub(crate) async fn find_first_user_totp_enabled(db: &DbPool) -> Result<bool, StatusCode> {
    let row = lyra_user::Entity::find()
        .limit(1)
        .into_partial_model::<TotpFlagRow>()
        .one(&db.orm())
        .await
        .map_err(|e| {
            tracing::error!("DB error: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    Ok(row.is_some_and(|r| r.totp_enabled))
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
    let id = id_bind_value(db, id)?;
    // Column-level insert (not an ActiveModel): the entity PK is `Uuid`,
    // while SQLite rows carry legacy TEXT ids that only a raw value can
    // express. Columns and table come from the entity, so the statement
    // cannot drift from the schema.
    let insert = InsertStatement::new()
        .into_table(lyra_user::Entity)
        .columns([
            lyra_user::Column::Id,
            lyra_user::Column::Username,
            lyra_user::Column::PasswordHash,
            lyra_user::Column::DisplayName,
            lyra_user::Column::Locale,
            lyra_user::Column::EncryptedDek,
        ])
        .values([
            Expr::val(id),
            Expr::val(username),
            Expr::val(password_hash),
            Expr::val(display_name),
            Expr::val(locale),
            Expr::val(encrypted_dek),
        ])
        .map_err(|e| sqlx::Error::Protocol(e.to_string()))?
        .to_owned();
    db.orm().execute(&insert).await.map_err(dberr_to_sqlx)?;
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
    let row = lyra_user::Entity::find()
        .filter(lyra_user::Column::Username.eq(username))
        .into_partial_model::<UserRow>()
        .one(&db.orm())
        .await
        .map_err(|e| {
            tracing::error!("DB error finding user: {e}");
            AuthError::internal("Authentication failed")
        })?;
    Ok(row.map(UserData::from))
}

pub(crate) async fn find_user_by_id(
    db: &DbPool,
    user_id: &str,
) -> Result<Option<UserData>, AuthError> {
    let Ok(id) = id_bind_value(db, user_id) else {
        return Ok(None);
    };
    let row = lyra_user::Entity::find()
        .filter(lyra_user::Column::Id.eq(id))
        .into_partial_model::<UserRow>()
        .one(&db.orm())
        .await
        .map_err(|e| {
            tracing::error!("DB error finding user by ID: {e}");
            AuthError::internal("Failed to look up user")
        })?;
    Ok(row.map(UserData::from))
}

pub(crate) async fn update_user_totp(
    db: &DbPool,
    user_id: &str,
    totp_secret: Option<&str>,
    totp_enabled: bool,
) -> Result<(), StatusCode> {
    let id = id_bind_value(db, user_id).map_err(|_| StatusCode::NOT_FOUND)?;
    lyra_user::Entity::update_many()
        .col_expr(lyra_user::Column::TotpSecret, Expr::val(totp_secret))
        .col_expr(lyra_user::Column::TotpEnabled, Expr::val(totp_enabled))
        .col_expr(lyra_user::Column::UpdatedAt, Expr::current_timestamp())
        .filter(lyra_user::Column::Id.eq(id))
        .exec(&db.orm())
        .await
        .map_err(|e| {
            tracing::error!("Failed to update TOTP: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    Ok(())
}

pub(crate) async fn update_user_password(
    db: &DbPool,
    user_id: &str,
    password_hash: &str,
) -> Result<(), StatusCode> {
    let id = id_bind_value(db, user_id).map_err(|_| StatusCode::NOT_FOUND)?;
    lyra_user::Entity::update_many()
        .col_expr(lyra_user::Column::PasswordHash, Expr::val(password_hash))
        .col_expr(lyra_user::Column::UpdatedAt, Expr::current_timestamp())
        .filter(lyra_user::Column::Id.eq(id))
        .exec(&db.orm())
        .await
        .map_err(|e| {
            tracing::error!("Failed to update password: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    Ok(())
}
