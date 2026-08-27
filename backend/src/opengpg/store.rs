//! Persist OpenGPG keys (armored, passphrase-locked secrets at rest).

use chrono::{DateTime, Utc};
use sea_orm::sea_query::{Expr, Order, Query, SelectStatement};
use sea_orm::{ColumnTrait, ConnectionTrait, QueryResult, Value};
use uuid::Uuid;

use super::keys::{OpengpgError, ParsedKey, parse_armored_key, public_armored_from_stored};
use crate::db_row::{IdParam, id_param};
use crate::entities::opengpg_key;
use crate::storage::DbPool;

/// Stored key row (never includes unlocked secret material).
#[derive(Debug, Clone)]
pub struct StoredKey {
    pub id: String,
    #[allow(dead_code)] // ownership; reserved for future multi-user checks
    pub user_id: String,
    pub fingerprint: String,
    pub primary_email: String,
    pub emails: Vec<String>,
    pub is_secret: bool,
    pub is_primary: bool,
    pub revoked: bool,
    /// Armored key data (secret keys stay passphrase-locked).
    pub key_data: String,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
}

fn map_db_err(e: sqlx::Error) -> OpengpgError {
    if let sqlx::Error::Database(db) = &e {
        let msg = db.message();
        if msg.contains("UNIQUE") || msg.contains("unique") || msg.contains("duplicate") {
            return OpengpgError::Conflict("fingerprint already imported".into());
        }
    }
    OpengpgError::Database(e)
}

/// Map a SeaORM error onto [`OpengpgError`].
///
/// SeaORM's portable unique-violation classification keeps the conflict
/// mapping the old sqlx-message sniffing implemented (including when the
/// wrapped driver error cannot be unwrapped); everything else falls through
/// to [`map_db_err`] with the recovered `sqlx::Error`.
fn orm_err(err: sea_orm::DbErr) -> OpengpgError {
    if matches!(
        err.sql_err(),
        Some(sea_orm::SqlErr::UniqueConstraintViolation(_))
    ) {
        return OpengpgError::Conflict("fingerprint already imported".into());
    }
    let sqlx_err = unwrap_sqlx_err(err);
    map_db_err(sqlx_err)
}

/// Recover the `sqlx::Error` SeaORM wrapped, so existing sqlx-based error
/// reporting (including `map_db_err`'s message sniffing) keeps working.
fn unwrap_sqlx_err(err: sea_orm::DbErr) -> sqlx::Error {
    use sea_orm::RuntimeErr;
    match err {
        sea_orm::DbErr::Exec(RuntimeErr::SqlxError(e))
        | sea_orm::DbErr::Query(RuntimeErr::SqlxError(e))
        | sea_orm::DbErr::Conn(RuntimeErr::SqlxError(e)) => std::sync::Arc::try_unwrap(e)
            .unwrap_or_else(|shared| sqlx::Error::Protocol(shared.to_string())),
        other => sqlx::Error::Protocol(other.to_string()),
    }
}

/// Dialect-aware bind for a UUID-column value: TEXT on SQLite, native UUID on
/// Postgres (`IdParam` holds the parse semantics — any text on SQLite).
fn id_value(db: &DbPool, id: &str, what: &str) -> Result<Value, OpengpgError> {
    let bind = id_param(db, id).map_err(|_| OpengpgError::InvalidInput(format!("{what} id")))?;
    Ok(match bind {
        IdParam::Text(s) => Value::String(Some(s)),
        IdParam::Uuid(u) => Value::Uuid(Some(u)),
    })
}

/// `updated_at` write, shaped like the legacy `datetime('now')` / `NOW()`
/// defaults so sqlite rows keep their `YYYY-MM-DD HH:MM:SS` text format.
fn now_value(db: &DbPool) -> Value {
    match db {
        DbPool::Sqlite(_) => {
            Value::String(Some(Utc::now().format("%Y-%m-%d %H:%M:%S").to_string()))
        }
        #[cfg(feature = "postgres")]
        DbPool::Postgres(_) => Value::ChronoDateTimeUtc(Some(Utc::now())),
    }
}

/// Decode a UUID/TEXT id column: `String` on SQLite, native UUID on Postgres.
fn row_id(row: &QueryResult, col: &str) -> Result<String, sea_orm::DbErr> {
    if let Some(s) = row.try_get::<Option<String>>("", col).ok().flatten() {
        return Ok(s);
    }
    row.try_get::<Option<Uuid>>("", col)?
        .map(|u| u.to_string())
        .ok_or_else(|| missing_column(col))
}

/// Nullable timestamp column: stored text on SQLite, RFC3339 on Postgres.
fn row_opt_ts(row: &QueryResult, col: &str) -> Result<Option<String>, sea_orm::DbErr> {
    if let Ok(text) = row.try_get::<Option<String>>("", col) {
        return Ok(text);
    }
    row.try_get::<Option<DateTime<Utc>>>("", col)
        .map(|opt| opt.map(|t| t.to_rfc3339()))
}

fn missing_column(col: &str) -> sea_orm::DbErr {
    sea_orm::DbErr::Query(sea_orm::RuntimeErr::Internal(format!(
        "missing column {col}"
    )))
}

/// Add every `opengpg_key` column `StoredKey` needs to a select statement.
fn add_key_columns(query: &mut SelectStatement) {
    query
        .column(opengpg_key::Column::Id)
        .column(opengpg_key::Column::UserId)
        .column(opengpg_key::Column::Fingerprint)
        .column(opengpg_key::Column::PrimaryEmail)
        .column(opengpg_key::Column::Emails)
        .column(opengpg_key::Column::IsSecret)
        .column(opengpg_key::Column::IsPrimary)
        .column(opengpg_key::Column::Revoked)
        .column(opengpg_key::Column::KeyData)
        .column(opengpg_key::Column::CreatedAt)
        .column(opengpg_key::Column::UpdatedAt);
}

fn stored_key_from_row(row: &QueryResult) -> Result<StoredKey, sea_orm::DbErr> {
    let emails_json = row
        .try_get::<Option<serde_json::Value>>("", "emails")
        .ok()
        .flatten()
        .unwrap_or(serde_json::Value::Array(vec![]));
    Ok(StoredKey {
        id: row_id(row, "id")?,
        user_id: row_id(row, "user_id")?,
        fingerprint: row.try_get("", "fingerprint")?,
        primary_email: row.try_get("", "primary_email")?,
        emails: serde_json::from_value(emails_json).unwrap_or_default(),
        is_secret: row.try_get("", "is_secret")?,
        is_primary: row.try_get("", "is_primary")?,
        revoked: row.try_get("", "revoked")?,
        key_data: row.try_get("", "key_data")?,
        created_at: row_opt_ts(row, "created_at")?,
        updated_at: row_opt_ts(row, "updated_at")?,
    })
}

/// Insert a newly parsed key for `user_id`.
pub async fn insert_key(
    db: &DbPool,
    user_id: &str,
    parsed: &ParsedKey,
    is_primary: bool,
) -> Result<StoredKey, OpengpgError> {
    let id = Uuid::new_v7(uuid::Timestamp::now(uuid::NoContext)).to_string();
    let user = id_value(db, user_id, "user")?;
    let key = id_value(db, &id, "key")?;
    let emails = serde_json::Value::Array(
        parsed
            .emails
            .iter()
            .cloned()
            .map(serde_json::Value::String)
            .collect(),
    );
    let conn = db.orm();

    if is_primary {
        // Demote any current primary before inserting the new one.
        let mut demote = Query::update();
        demote
            .table(opengpg_key::Entity)
            .value(opengpg_key::Column::IsPrimary, false)
            .value(opengpg_key::Column::UpdatedAt, now_value(db))
            .and_where(opengpg_key::Column::UserId.eq(user.clone()));
        conn.execute(&demote).await.map_err(orm_err)?;
    }

    let mut insert = Query::insert();
    insert
        .into_table(opengpg_key::Entity)
        .columns([
            opengpg_key::Column::Id,
            opengpg_key::Column::UserId,
            opengpg_key::Column::Fingerprint,
            opengpg_key::Column::PrimaryEmail,
            opengpg_key::Column::Emails,
            opengpg_key::Column::IsSecret,
            opengpg_key::Column::IsPrimary,
            opengpg_key::Column::Revoked,
            opengpg_key::Column::KeyData,
        ])
        .values_panic([
            key.into(),
            user.into(),
            parsed.fingerprint.clone().into(),
            parsed.primary_email.clone().into(),
            Expr::val(Value::Json(Some(Box::new(emails)))),
            parsed.is_secret.into(),
            is_primary.into(),
            parsed.revoked.into(),
            parsed.key_data.clone().into(),
        ]);
    conn.execute(&insert).await.map_err(orm_err)?;

    get_key(db, user_id, &id)
        .await?
        .ok_or(OpengpgError::NotFound)
}

/// Parse armored input and insert.
pub async fn import_armored(
    db: &DbPool,
    user_id: &str,
    armored: &str,
    is_primary: bool,
) -> Result<StoredKey, OpengpgError> {
    let parsed = parse_armored_key(armored)?;
    insert_key(db, user_id, &parsed, is_primary).await
}

pub async fn list_keys(db: &DbPool, user_id: &str) -> Result<Vec<StoredKey>, OpengpgError> {
    let user = id_value(db, user_id, "user")?;
    let mut query = Query::select();
    add_key_columns(&mut query);
    query
        .from(opengpg_key::Entity)
        .and_where(opengpg_key::Column::UserId.eq(user))
        .order_by(opengpg_key::Column::IsPrimary, Order::Desc)
        .order_by(opengpg_key::Column::PrimaryEmail, Order::Asc);

    let rows = db.orm().query_all(&query).await.map_err(orm_err)?;
    rows.iter()
        .map(stored_key_from_row)
        .collect::<Result<Vec<_>, _>>()
        .map_err(orm_err)
}

pub async fn get_key(
    db: &DbPool,
    user_id: &str,
    key_id: &str,
) -> Result<Option<StoredKey>, OpengpgError> {
    let user = id_value(db, user_id, "user")?;
    let key = id_value(db, key_id, "key")?;
    let mut query = Query::select();
    add_key_columns(&mut query);
    query
        .from(opengpg_key::Entity)
        .and_where(opengpg_key::Column::Id.eq(key))
        .and_where(opengpg_key::Column::UserId.eq(user));

    let row = db.orm().query_one(&query).await.map_err(orm_err)?;
    row.map(|r| stored_key_from_row(&r))
        .transpose()
        .map_err(orm_err)
}

/// Return armored key_data for export (caller enforces re-auth for secrets).
pub async fn export_armored(
    db: &DbPool,
    user_id: &str,
    key_id: &str,
) -> Result<String, OpengpgError> {
    let key = get_key(db, user_id, key_id)
        .await?
        .ok_or(OpengpgError::NotFound)?;
    Ok(key.key_data)
}

/// Public certificate only (never secret material).
pub async fn export_public_armored(
    db: &DbPool,
    user_id: &str,
    key_id: &str,
) -> Result<String, OpengpgError> {
    let key = get_key(db, user_id, key_id)
        .await?
        .ok_or(OpengpgError::NotFound)?;
    public_armored_from_stored(&key.key_data)
}

/// Promote `key_id` to primary (clears other primaries for the user).
pub async fn set_primary(
    db: &DbPool,
    user_id: &str,
    key_id: &str,
) -> Result<StoredKey, OpengpgError> {
    let _ = get_key(db, user_id, key_id)
        .await?
        .ok_or(OpengpgError::NotFound)?;
    let user = id_value(db, user_id, "user")?;
    let key = id_value(db, key_id, "key")?;
    let conn = db.orm();

    let mut demote = Query::update();
    demote
        .table(opengpg_key::Entity)
        .value(opengpg_key::Column::IsPrimary, false)
        .value(opengpg_key::Column::UpdatedAt, now_value(db))
        .and_where(opengpg_key::Column::UserId.eq(user.clone()));
    conn.execute(&demote).await.map_err(orm_err)?;

    let mut promote = Query::update();
    promote
        .table(opengpg_key::Entity)
        .value(opengpg_key::Column::IsPrimary, true)
        .value(opengpg_key::Column::UpdatedAt, now_value(db))
        .and_where(opengpg_key::Column::Id.eq(key))
        .and_where(opengpg_key::Column::UserId.eq(user));
    conn.execute(&promote).await.map_err(orm_err)?;

    get_key(db, user_id, key_id)
        .await?
        .ok_or(OpengpgError::NotFound)
}

pub async fn delete_key(db: &DbPool, user_id: &str, key_id: &str) -> Result<(), OpengpgError> {
    let existing = get_key(db, user_id, key_id)
        .await?
        .ok_or(OpengpgError::NotFound)?;
    if existing.is_primary {
        return Err(OpengpgError::InvalidInput(
            "refuse deleting primary key; promote another first".into(),
        ));
    }
    let user = id_value(db, user_id, "user")?;
    let key = id_value(db, key_id, "key")?;
    let mut delete = Query::delete();
    delete
        .from_table(opengpg_key::Entity)
        .and_where(opengpg_key::Column::Id.eq(key))
        .and_where(opengpg_key::Column::UserId.eq(user));
    db.orm().execute(&delete).await.map_err(orm_err)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::opengpg::keys::tests_support::gen_test_secret_armor;
    use crate::storage::create_test_state;
    use uuid::Uuid;

    #[tokio::test]
    async fn import_list_export_roundtrip() {
        let state = create_test_state().await;
        let db = &state.db;
        let user_id = Uuid::new_v7(uuid::Timestamp::now(uuid::NoContext)).to_string();
        db_execute!(
            db,
            "INSERT INTO lyra_user (id, username, password_hash) VALUES (?, ?, ?)",
            &id_param(db, &user_id).unwrap(),
            "opengpg-tester",
            "hash"
        )
        .unwrap();

        let armor = gen_test_secret_armor(Some("s3cret"));
        let stored = import_armored(db, &user_id, &armor, true)
            .await
            .expect("import");
        assert!(stored.is_secret);
        assert!(stored.is_primary);
        assert_eq!(stored.primary_email, "test@example.com");

        let listed = list_keys(db, &user_id).await.expect("list");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].fingerprint, stored.fingerprint);

        let exported = export_armored(db, &user_id, &stored.id)
            .await
            .expect("export");
        assert_eq!(exported, stored.key_data);

        // Primary delete refused
        let err = delete_key(db, &user_id, &stored.id).await.unwrap_err();
        assert!(matches!(err, OpengpgError::InvalidInput(_)));
    }
}
