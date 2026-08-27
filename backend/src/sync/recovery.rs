//! Sync failure recovery helpers (§10 failure matrix).
//!
//! Credential decrypt failures deactivate the account so the UI can prompt
//! for re-entry. Oversized bodies are skipped and flagged `fetch_error`.

#![allow(clippy::doc_markdown)]

use sea_orm::sea_query::{Expr, Query as Sq};
use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QueryResult, Value};

use super::types::SyncError;
use crate::entities::{mail_account, message};
use crate::storage::DbPool;

/// Soft cap for lazy BODY fetch (25 MiB). Larger messages are skipped.
pub(crate) const MAX_MESSAGE_BODY_BYTES: u64 = 25 * 1024 * 1024;

// ── SeaORM plumbing ──────────────────────────────────────────────────
//
// Ids are TEXT on SQLite and native UUID on Postgres; `IdParam` keeps the
// parse semantics the macro layer used. `updated_at` writes go through
// `CURRENT_TIMESTAMP`, which matches the engine defaults for both pools.

/// Unwrap the driver error SeaORM wraps so [`SyncError::Database`] keeps
/// reporting the underlying `sqlx::Error`; non-driver SeaORM errors become
/// `sqlx::Error::Protocol` with the original message.
fn orm_err(err: sea_orm::DbErr) -> SyncError {
    use sea_orm::RuntimeErr;
    let sqlx_err = match err {
        sea_orm::DbErr::Exec(RuntimeErr::SqlxError(e))
        | sea_orm::DbErr::Query(RuntimeErr::SqlxError(e))
        | sea_orm::DbErr::Conn(RuntimeErr::SqlxError(e)) => std::sync::Arc::try_unwrap(e)
            .unwrap_or_else(|shared| sqlx::Error::Protocol(shared.to_string())),
        other => sqlx::Error::Protocol(other.to_string()),
    };
    SyncError::Database(sqlx_err)
}

/// Dialect-aware bind for a UUID-column id: TEXT on SQLite, native UUID on
/// Postgres — the same split `id_param` makes for the macro layer.
fn id_value(db: &DbPool, id: &str) -> Result<Value, SyncError> {
    use crate::db_row::{IdParam, InvalidIdError, id_param};
    Ok(
        match id_param(db, id)
            .map_err(|e: InvalidIdError| SyncError::Database(sqlx::Error::from(e)))?
        {
            IdParam::Text(s) => Value::String(Some(s)),
            IdParam::Uuid(u) => Value::Uuid(Some(u)),
        },
    )
}

/// Nullable JSON column: stored text on SQLite, JSONB on Postgres — both
/// normalize to the text form the flags merge operates on.
fn row_opt_json_text(row: &QueryResult, col: &str) -> Result<Option<String>, SyncError> {
    if let Ok(text) = row.try_get::<Option<String>>("", col) {
        return Ok(text);
    }
    row.try_get::<Option<serde_json::Value>>("", col)
        .map(|opt| opt.map(|v| v.to_string()))
        .map_err(orm_err)
}

/// Bind flag JSON text: raw text on SQLite, parsed `JSONB` value on Postgres
/// (`JsonParam::lenient` semantics so non-JSON strings still bind).
fn json_value(db: &DbPool, raw: &str) -> Value {
    match db {
        crate::storage::DbPool::Sqlite(_) => Value::String(Some(raw.to_owned())),
        #[cfg(feature = "postgres")]
        crate::storage::DbPool::Postgres(_) => {
            let v = serde_json::from_str(raw)
                .unwrap_or_else(|_| serde_json::Value::String(raw.to_owned()));
            Value::Json(Some(Box::new(v)))
        }
    }
}

/// Mark a mail account inactive (credential / auth recovery path).
pub(crate) async fn deactivate_account(db: &DbPool, account_id: &str) -> Result<(), SyncError> {
    mail_account::Entity::update_many()
        .col_expr(mail_account::Column::IsActive, false.into())
        .col_expr(mail_account::Column::SyncEnabled, false.into())
        .col_expr(mail_account::Column::UpdatedAt, Expr::current_timestamp())
        .filter(mail_account::Column::Id.eq(id_value(db, account_id)?))
        .exec(&db.orm())
        .await
        .map_err(orm_err)?;
    tracing::warn!(
        account_id,
        "deactivated mail account after credential failure"
    );
    Ok(())
}

/// Decrypt failure → deactivate account and return a secret-free [`SyncError::Crypto`].
pub(crate) async fn fail_credential_decrypt(db: &DbPool, account_id: &str) -> SyncError {
    if let Err(error) = deactivate_account(db, account_id).await {
        tracing::error!(account_id, %error, "failed to deactivate account after decrypt error");
    }
    SyncError::Crypto("credential decrypt failed; re-enter account password".into())
}

/// Record a per-message fetch failure in `flags.fetch_error` without aborting sync.
pub(crate) async fn mark_message_fetch_error(
    db: &DbPool,
    message_id: &str,
    reason: &str,
) -> Result<(), SyncError> {
    let id = id_value(db, message_id)?;
    let conn = db.orm();

    let mut stmt = Sq::select();
    stmt.column(message::Column::Flags)
        .from(message::Entity)
        .and_where(message::Column::Id.eq(id.clone()));
    let row = conn.query_one(&stmt).await.map_err(orm_err)?;
    let existing: Option<String> = match row {
        Some(r) => row_opt_json_text(&r, "flags")?,
        None => None,
    };

    let mut map = existing
        .as_deref()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();
    map.insert(
        "fetch_error".into(),
        serde_json::Value::String(reason.to_owned()),
    );
    let flags = serde_json::to_string(&map).unwrap_or_else(|_| {
        format!(
            r#"{{"fetch_error":{}}}"#,
            serde_json::to_string(reason).unwrap_or_default()
        )
    });

    message::Entity::update_many()
        .col_expr(message::Column::Flags, json_value(db, &flags).into())
        .col_expr(message::Column::UpdatedAt, Expr::current_timestamp())
        .filter(message::Column::Id.eq(id))
        .exec(&conn)
        .await
        .map_err(orm_err)?;
    Ok(())
}

/// True when `size_bytes` exceeds the lazy-fetch soft cap.
#[must_use]
pub(crate) fn body_exceeds_limit(size_bytes: Option<i64>) -> bool {
    match size_bytes {
        Some(s) if s > 0 => u64::try_from(s).is_ok_and(|n| n > MAX_MESSAGE_BODY_BYTES),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn body_limit_rejects_oversize() {
        assert!(!body_exceeds_limit(None));
        assert!(!body_exceeds_limit(Some(1024)));
        assert!(body_exceeds_limit(Some(
            i64::try_from(MAX_MESSAGE_BODY_BYTES + 1).unwrap()
        )));
    }
}
