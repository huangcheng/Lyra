//! Persist OpenGPG keys (armored, passphrase-locked secrets at rest).

use chrono::{DateTime, Utc};
use sea_orm::sea_query::{Expr, Order, Query, SelectStatement};
use sea_orm::{ColumnTrait, ConnectionTrait, QueryResult, Value};
use uuid::Uuid;

use super::keys::{OpengpgError, ParsedKey, parse_armored_key, public_armored_from_stored};
use crate::db_row::{IdParam, id_param};
use crate::entities::{mail_account, opengpg_key};
use crate::storage::DbPool;

/// Stored key row (never includes unlocked secret material).
#[derive(Debug, Clone)]
pub struct StoredKey {
    pub id: String,
    #[allow(dead_code)] // ownership; reserved for future multi-user checks
    pub user_id: String,
    /// Owning mail account; `None` = shared contact / legacy unbound key.
    pub account_id: Option<String>,
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

/// Mail-account projection needed when binding a key to an identity.
#[derive(Debug, Clone)]
pub struct OwnedAccount {
    pub email_address: String,
    pub display_name: Option<String>,
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
    row_opt_id(row, col)?.ok_or_else(|| missing_column(col))
}

/// Nullable UUID/TEXT id column.
fn row_opt_id(row: &QueryResult, col: &str) -> Result<Option<String>, sea_orm::DbErr> {
    if let Some(s) = row.try_get::<Option<String>>("", col).ok().flatten() {
        return Ok(Some(s));
    }
    row.try_get::<Option<Uuid>>("", col)
        .map(|opt| opt.map(|u| u.to_string()))
}

/// Nullable timestamp column: stored text on SQLite, RFC3339 on Postgres.
fn row_opt_ts(row: &QueryResult, col: &str) -> Result<Option<String>, sea_orm::DbErr> {
    if let Ok(text) = row.try_get::<Option<String>>("", col) {
        return Ok(text.map(crate::db_row::normalize_ts_text));
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
        .column(opengpg_key::Column::AccountId)
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

/// Account-bucket predicate: `Some` = rows bound to that account, `None` =
/// unbound (NULL) rows only (contact/legacy keys).
fn bucket_where(db: &DbPool, account_id: Option<&str>) -> Result<Expr, OpengpgError> {
    Ok(match account_id {
        Some(a) => opengpg_key::Column::AccountId.eq(id_value(db, a, "account")?),
        None => opengpg_key::Column::AccountId.is_null(),
    })
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
        account_id: row_opt_id(row, "account_id")?,
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

/// Insert a newly parsed key for `user_id`, optionally bound to a mail
/// account. Enforces the identity model here so every caller path gets it:
/// secret keys must bind to an owned account whose address they carry.
pub async fn insert_key(
    db: &DbPool,
    user_id: &str,
    parsed: &ParsedKey,
    is_primary: bool,
    account_id: Option<&str>,
) -> Result<StoredKey, OpengpgError> {
    match account_id {
        Some(a) => {
            validate_account_binding(db, user_id, a, &parsed.emails, parsed.is_secret).await?;
        }
        None if parsed.is_secret => {
            return Err(OpengpgError::InvalidInput(
                "identity (secret) keys must be bound to one of your accounts".into(),
            ));
        }
        None => {}
    }
    let id = Uuid::new_v7(uuid::Timestamp::now(uuid::NoContext)).to_string();
    let user = id_value(db, user_id, "user")?;
    let key = id_value(db, &id, "key")?;
    let account_bind = match account_id {
        Some(a) => Some(id_value(db, a, "account")?),
        None => None,
    };
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
        // Demote any current primary in the same (user, account) bucket.
        let mut demote = Query::update();
        demote
            .table(opengpg_key::Entity)
            .value(opengpg_key::Column::IsPrimary, false)
            .value(opengpg_key::Column::UpdatedAt, now_value(db))
            .and_where(opengpg_key::Column::UserId.eq(user.clone()))
            .and_where(bucket_where(db, account_id)?);
        conn.execute(&demote).await.map_err(orm_err)?;
    }

    let mut insert = Query::insert();
    insert.into_table(opengpg_key::Entity);
    let mut columns = vec![
        opengpg_key::Column::Id,
        opengpg_key::Column::UserId,
        opengpg_key::Column::Fingerprint,
        opengpg_key::Column::PrimaryEmail,
        opengpg_key::Column::Emails,
        opengpg_key::Column::IsSecret,
        opengpg_key::Column::IsPrimary,
        opengpg_key::Column::Revoked,
        opengpg_key::Column::KeyData,
    ];
    let mut values: Vec<Expr> = vec![
        key.into(),
        user.into(),
        parsed.fingerprint.clone().into(),
        parsed.primary_email.clone().into(),
        Expr::val(Value::Json(Some(Box::new(emails)))),
        parsed.is_secret.into(),
        is_primary.into(),
        parsed.revoked.into(),
        parsed.key_data.clone().into(),
    ];
    if let Some(bind) = account_bind {
        columns.push(opengpg_key::Column::AccountId);
        values.push(bind.into());
    }
    insert.columns(columns).values_panic(values);
    conn.execute(&insert).await.map_err(orm_err)?;

    get_key(db, user_id, &id)
        .await?
        .ok_or(OpengpgError::NotFound)
}

/// Parse armored input and insert; account-binding policy is enforced by
/// [`insert_key`].
pub async fn import_armored(
    db: &DbPool,
    user_id: &str,
    armored: &str,
    is_primary: bool,
    account_id: Option<&str>,
) -> Result<StoredKey, OpengpgError> {
    let parsed = parse_armored_key(armored)?;
    insert_key(db, user_id, &parsed, is_primary, account_id).await
}

/// List keys for a user; optionally only those bound to one account.
pub async fn list_keys(
    db: &DbPool,
    user_id: &str,
    account_id: Option<&str>,
) -> Result<Vec<StoredKey>, OpengpgError> {
    let user = id_value(db, user_id, "user")?;
    let mut query = Query::select();
    add_key_columns(&mut query);
    query
        .from(opengpg_key::Entity)
        .and_where(opengpg_key::Column::UserId.eq(user));
    if let Some(a) = account_id {
        query.and_where(bucket_where(db, Some(a))?);
    }
    query
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

/// Load a mail account owned by `user_id`, for OpenGPG key binding.
pub async fn load_owned_account(
    db: &DbPool,
    user_id: &str,
    account_id: &str,
) -> Result<OwnedAccount, OpengpgError> {
    use sea_orm::{EntityTrait, QueryFilter, QuerySelect};

    let user = id_value(db, user_id, "user")?;
    let acct = id_value(db, account_id, "account")?;
    let rows = mail_account::Entity::find()
        .select_only()
        .column(mail_account::Column::EmailAddress)
        .column(mail_account::Column::DisplayName)
        .filter(mail_account::Column::Id.eq(acct))
        .filter(mail_account::Column::UserId.eq(user))
        .into_tuple::<(String, Option<String>)>()
        .all(&db.orm())
        .await
        .map_err(|e| OpengpgError::Database(unwrap_sqlx_err(e)))?;
    let mut it = rows.into_iter();
    it.next()
        .map(|(email_address, display_name)| OwnedAccount {
            email_address,
            display_name,
        })
        .ok_or_else(|| {
            OpengpgError::InvalidInput("accountId does not reference one of your accounts".into())
        })
}

/// Check a key may be bound to `owned`: identity (secret) keys must carry the
/// account address as one of their UIDs; public keys bind freely.
pub fn ensure_emails_match_account(
    emails: &[String],
    is_secret: bool,
    owned: &OwnedAccount,
) -> Result<(), OpengpgError> {
    if !is_secret
        || emails
            .iter()
            .any(|e| e.eq_ignore_ascii_case(&owned.email_address))
    {
        return Ok(());
    }
    Err(OpengpgError::InvalidInput(format!(
        "secret key must carry the account address {}",
        owned.email_address
    )))
}

/// Validate + load an account binding for a key with these UID emails.
pub async fn validate_account_binding(
    db: &DbPool,
    user_id: &str,
    account_id: &str,
    key_emails: &[String],
    is_secret: bool,
) -> Result<OwnedAccount, OpengpgError> {
    let owned = load_owned_account(db, user_id, account_id).await?;
    ensure_emails_match_account(key_emails, is_secret, &owned)?;
    Ok(owned)
}

/// Bind / rebind / unbind a key. Secret keys cannot be unbound (delete
/// instead); rebinding re-checks the account-UID match for secret keys.
/// The primary flag moves with the key into its new bucket untouched.
pub async fn set_account_binding(
    db: &DbPool,
    user_id: &str,
    key_id: &str,
    new_account_id: Option<&str>,
) -> Result<StoredKey, OpengpgError> {
    let existing = get_key(db, user_id, key_id)
        .await?
        .ok_or(OpengpgError::NotFound)?;
    if new_account_id.is_none() && existing.is_secret {
        return Err(OpengpgError::InvalidInput(
            "identity keys stay bound to their account; delete the key instead".into(),
        ));
    }
    let account_bind = match new_account_id {
        Some(a) => {
            let _ = validate_account_binding(db, user_id, a, &existing.emails, existing.is_secret)
                .await?;
            Some(id_value(db, a, "account")?)
        }
        None => None,
    };
    let user = id_value(db, user_id, "user")?;
    let key = id_value(db, key_id, "key")?;
    let mut update = Query::update();
    update
        .table(opengpg_key::Entity)
        .value(opengpg_key::Column::UpdatedAt, now_value(db))
        .and_where(opengpg_key::Column::Id.eq(key))
        .and_where(opengpg_key::Column::UserId.eq(user));
    update.value(
        opengpg_key::Column::AccountId,
        match account_bind {
            Some(bind) => bind.into(),
            None => Expr::null(),
        },
    );
    db.orm().execute(&update).await.map_err(orm_err)?;
    get_key(db, user_id, key_id)
        .await?
        .ok_or(OpengpgError::NotFound)
}

/// Promote `key_id` to primary within its own (user, account) bucket.
pub async fn set_primary(
    db: &DbPool,
    user_id: &str,
    key_id: &str,
) -> Result<StoredKey, OpengpgError> {
    let existing = get_key(db, user_id, key_id)
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
        .and_where(opengpg_key::Column::UserId.eq(user.clone()))
        .and_where(bucket_where(db, existing.account_id.as_deref())?);
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
    use crate::opengpg::keys::public_armored_from_stored;
    use crate::opengpg::keys::tests_support::gen_test_secret_armor;
    use crate::storage::create_test_state;
    use uuid::Uuid;

    fn fresh_id() -> String {
        Uuid::new_v7(uuid::Timestamp::now(uuid::NoContext)).to_string()
    }

    async fn seed_user(db: &DbPool) -> String {
        let user_id = fresh_id();
        let stmt = sea_orm::Statement::from_sql_and_values(
            db.backend(),
            "INSERT INTO lyra_user (id, username, password_hash) VALUES (?, ?, ?)",
            [
                id_value(db, &user_id, "user").unwrap(),
                sea_orm::Value::from(format!("opengpg-{user_id}")),
                sea_orm::Value::from("hash"),
            ],
        );
        db.orm().query_one_raw(stmt).await.unwrap();
        user_id
    }

    async fn seed_account(db: &DbPool, user_id: &str, email_address: &str) -> String {
        let id = fresh_id();
        let stmt = sea_orm::Statement::from_sql_and_values(
            db.backend(),
            "INSERT INTO mail_account (id, user_id, display_name, email_address, protocol, \
             auth_type, credential, is_active, sync_enabled, receive_protocol, send_protocol) \
             VALUES (?, ?, NULL, ?, 'imap', 'password', 'enc', 1, 0, 'imap', 'smtp')",
            [
                id_value(db, &id, "account").unwrap(),
                id_value(db, user_id, "user").unwrap(),
                sea_orm::Value::from(email_address.to_string()),
            ],
        );
        db.orm().query_one_raw(stmt).await.unwrap();
        id
    }

    #[tokio::test]
    async fn import_list_export_roundtrip_with_binding() {
        let state = create_test_state().await;
        let db = &state.db;
        let user_id = seed_user(db).await;
        // Test armor always carries test@example.com.
        let acct_a = seed_account(db, &user_id, "test@example.com").await;
        let acct_b = seed_account(db, &user_id, "other@example.com").await;

        let armor = gen_test_secret_armor(Some("s3cret"));
        let stored = import_armored(db, &user_id, &armor, true, Some(&acct_a))
            .await
            .expect("import");
        assert!(stored.is_secret);
        assert!(stored.is_primary);
        assert_eq!(stored.account_id.as_deref(), Some(acct_a.as_str()));
        assert_eq!(stored.primary_email, "test@example.com");

        let listed = list_keys(db, &user_id, None).await.expect("list");
        assert_eq!(listed.len(), 1);
        let in_bucket = list_keys(db, &user_id, Some(&acct_a)).await.expect("list");
        assert_eq!(in_bucket.len(), 1);
        let other_bucket = list_keys(db, &user_id, Some(&acct_b)).await.expect("list");
        assert!(other_bucket.is_empty());

        let exported = export_armored(db, &user_id, &stored.id)
            .await
            .expect("export");
        assert_eq!(exported, stored.key_data);

        // Primary delete refused
        let err = delete_key(db, &user_id, &stored.id).await.unwrap_err();
        assert!(matches!(err, OpengpgError::InvalidInput(_)));
    }

    #[tokio::test]
    async fn primaries_are_scoped_per_account() {
        let state = create_test_state().await;
        let db = &state.db;
        let user_id = seed_user(db).await;
        // Test armor always carries test@example.com; two accounts may hold it.
        let acct_a = seed_account(db, &user_id, "test@example.com").await;
        let acct_b = seed_account(db, &user_id, "test@example.com").await;

        let k_a = import_armored(
            db,
            &user_id,
            &gen_test_secret_armor(Some("p1")),
            true,
            Some(&acct_a),
        )
        .await
        .expect("a");
        let k_b = import_armored(
            db,
            &user_id,
            &gen_test_secret_armor(Some("p2")),
            true,
            Some(&acct_b),
        )
        .await
        .expect("b");

        // Each account keeps its own primary; no cross-account demotion.
        assert!(k_a.is_primary && k_b.is_primary);

        // Promoting another key into bucket b leaves bucket a untouched.
        let _ = import_armored(
            db,
            &user_id,
            &gen_test_secret_armor(Some("p3")),
            true,
            Some(&acct_b),
        )
        .await
        .expect("c");
        let b_keys = list_keys(db, &user_id, Some(&acct_b))
            .await
            .expect("list b");
        let still_primary = b_keys.iter().find(|k| k.id == k_b.id).expect("b row");
        assert!(!still_primary.is_primary);
        let a_keys = list_keys(db, &user_id, Some(&acct_a))
            .await
            .expect("list a");
        assert!(
            a_keys
                .iter()
                .find(|k| k.id == k_a.id)
                .expect("a row")
                .is_primary
        );
    }

    #[tokio::test]
    async fn unbound_secret_and_mismatched_address_are_rejected() {
        let state = create_test_state().await;
        let db = &state.db;
        let user_id = seed_user(db).await;

        // Secret without an account: refused by the identity model.
        let err = import_armored(
            db,
            &user_id,
            &gen_test_secret_armor(Some("pw")),
            false,
            None,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, OpengpgError::InvalidInput(_)));

        // Secret with an account whose address the key does not carry.
        let wrong = seed_account(db, &user_id, "someone-else@example.com").await;
        let err = import_armored(
            db,
            &user_id,
            &gen_test_secret_armor(Some("pw")),
            false,
            Some(&wrong),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, OpengpgError::InvalidInput(_)));
    }

    #[tokio::test]
    async fn contact_public_keys_bind_freely_but_secret_unbind_is_refused() {
        let state = create_test_state().await;
        let db = &state.db;
        let user_id = seed_user(db).await;
        let acct = seed_account(db, &user_id, "test@example.com").await;

        // Contact public key imports unbound.
        let secret_armor = gen_test_secret_armor(Some("pw"));
        let pub_armor = public_armored_from_stored(&secret_armor).unwrap();
        let contact = import_armored(db, &user_id, &pub_armor, false, None)
            .await
            .expect("public import");
        assert_eq!(contact.account_id, None);

        // …can be attached to an account afterwards…
        let rebound = set_account_binding(db, &user_id, &contact.id, Some(&acct))
            .await
            .expect("bind");
        assert_eq!(rebound.account_id.as_deref(), Some(acct.as_str()));
        // …and detached again (public keys may be shared).
        let detached = set_account_binding(db, &user_id, &contact.id, None)
            .await
            .expect("unbind");
        assert_eq!(detached.account_id, None);

        // Identity keys refuse detachment (different armor → distinct fingerprint).
        let identity_armor = gen_test_secret_armor(Some("pw2"));
        let identity = import_armored(db, &user_id, &identity_armor, true, Some(&acct))
            .await
            .expect("secret");
        let err = set_account_binding(db, &user_id, &identity.id, None)
            .await
            .unwrap_err();
        assert!(matches!(err, OpengpgError::InvalidInput(_)));

        // Cross-account rebind requires the UID address match: alias account has
        // a different address than the key's test@example.com.
        let alias = seed_account(db, &user_id, "other@example.com").await;
        let err = set_account_binding(db, &user_id, &identity.id, Some(&alias))
            .await
            .unwrap_err();
        assert!(matches!(err, OpengpgError::InvalidInput(_)));
    }
}
