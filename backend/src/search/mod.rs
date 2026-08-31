//! Full-text search over local message index (FTS5 / tsvector).
//!
//! See `docs/specs/2026-08-20-lyra-data-model-spec.md` §5.

#![allow(dead_code)] // seam wired in main; index/search callers land incrementally

use async_trait::async_trait;
use sea_orm::{ConnectionTrait, DbBackend, QueryResult, Statement, Value};
use std::sync::Arc;
use thiserror::Error;

use crate::db_row::id_param;
use crate::storage::DbPool;

/// One ranked search hit.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchHit {
    pub message_id: String,
    pub rank: f64,
}

/// Fields indexed for full-text search.
#[derive(Debug, Clone)]
pub struct MessageSearchDoc {
    pub id: String,
    pub account_id: String,
    pub subject: Option<String>,
    pub body_text: Option<String>,
    pub from_address: Option<String>,
}

#[derive(Debug, Error)]
pub enum SearchError {
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("invalid search query")]
    InvalidQuery,
    #[error("invalid id")]
    InvalidId(#[from] crate::db_row::InvalidIdError),
}

// ── SeaORM plumbing ──────────────────────────────────────────────────
//
// The engine-specific FTS SQL (FTS5 / tsvector) stays verbatim, but every
// statement is now built with an explicit backend tag and executed on the
// SeaORM connection (`db.orm()`), so the module no longer routes around the
// pool itself. Message ids are TEXT on SQLite and native UUID on Postgres;
// ids bind dialect-aware via `id_param`'s split.

/// Unwrap the driver error SeaORM wraps so [`SearchError::Database`] keeps
/// reporting the underlying `sqlx::Error`; non-driver SeaORM errors become
/// `sqlx::Error::Protocol` with the original message.
fn dberr_to_sqlx(err: sea_orm::DbErr) -> sqlx::Error {
    use sea_orm::RuntimeErr;
    match err {
        sea_orm::DbErr::Exec(RuntimeErr::SqlxError(e))
        | sea_orm::DbErr::Query(RuntimeErr::SqlxError(e))
        | sea_orm::DbErr::Conn(RuntimeErr::SqlxError(e)) => std::sync::Arc::try_unwrap(e)
            .unwrap_or_else(|shared| sqlx::Error::Protocol(shared.to_string())),
        other => sqlx::Error::Protocol(other.to_string()),
    }
}

fn orm_err(err: sea_orm::DbErr) -> SearchError {
    SearchError::from(dberr_to_sqlx(err))
}

/// Dialect-aware bind for a UUID-column value: TEXT on SQLite, native UUID on
/// Postgres.
fn id_value(db: &DbPool, id: &str) -> Result<Value, crate::db_row::InvalidIdError> {
    Ok(match id_param(db, id)? {
        crate::db_row::IdParam::Text(s) => Value::String(Some(s)),
        crate::db_row::IdParam::Uuid(u) => Value::Uuid(Some(u)),
    })
}

/// Optional variant of [`id_value`]; `None` binds a typed NULL so the
/// `(? IS NULL OR …)` filters stay usable.
fn opt_id_value(
    db: &DbPool,
    id: Option<&str>,
) -> Result<Option<Value>, crate::db_row::InvalidIdError> {
    id.map(|s| id_value(db, s)).transpose()
}

/// A bound-NULL stand-in when an optional id filter is absent.
fn value_or_null(v: Option<&Value>) -> Value {
    match v {
        Some(v) => v.clone(),
        None => Value::String(None),
    }
}

/// Decode a `message.id` column from either engine (TEXT on SQLite, UUID on
/// Postgres).
fn hit_from_row(row: &QueryResult) -> Result<SearchHit, SearchError> {
    let message_id = if let Ok(text) = row.try_get::<String>("", "message_id") {
        text
    } else {
        row.try_get::<uuid::Uuid>("", "message_id")
            .map_err(orm_err)?
            .to_string()
    };
    Ok(SearchHit {
        message_id,
        // SQLite's bm25() ranks are float8; Postgres ts_rank() is float4 —
        // decode whichever the engine returned.
        rank: row
            .try_get::<f64>("", "rank")
            .or_else(|_| row.try_get::<f32>("", "rank").map(f64::from))
            .map_err(orm_err)?,
    })
}

/// Engine-specific full-text search seam.
#[async_trait]
pub trait SearchIndex: Send + Sync {
    async fn index_message(&self, msg: &MessageSearchDoc) -> Result<(), SearchError>;
    async fn remove_message(&self, id: &str) -> Result<(), SearchError>;
    async fn search(
        &self,
        query: &str,
        account_id: &str,
        limit: usize,
    ) -> Result<Vec<SearchHit>, SearchError>;
}

/// Returns a [`SearchIndex`] for the active database engine.
pub fn search_index_for(db: &DbPool) -> Arc<dyn SearchIndex> {
    match db {
        DbPool::Sqlite(_) => Arc::new(SqliteSearchIndex { db: db.clone() }),
        #[cfg(feature = "postgres")]
        DbPool::Postgres(_) => Arc::new(PostgresSearchIndex { db: db.clone() }),
    }
}

const FTS_AVAILABLE_SQL_SQLITE: &str =
    "SELECT COUNT(*) AS c FROM sqlite_master WHERE type = 'table' AND name = 'message_fts'";

#[cfg(feature = "postgres")]
const FTS_AVAILABLE_SQL_POSTGRES: &str = r"
                SELECT COUNT(*) AS c
                FROM information_schema.columns
                WHERE table_schema = 'public'
                  AND table_name = 'message'
                  AND column_name = 'search_vector'
                ";

/// Whether migration 0009 has been applied (FTS table / tsvector column present).
pub async fn fts_available(db: &DbPool) -> Result<bool, sqlx::Error> {
    let stmt = match db.backend() {
        DbBackend::Sqlite => Statement::from_string(DbBackend::Sqlite, FTS_AVAILABLE_SQL_SQLITE),
        #[cfg(feature = "postgres")]
        DbBackend::Postgres => {
            Statement::from_string(DbBackend::Postgres, FTS_AVAILABLE_SQL_POSTGRES)
        }
        other => {
            return Err(sqlx::Error::Protocol(format!(
                "fts availability probe: unsupported backend {other:?}"
            )));
        }
    };
    let row = db
        .orm()
        .query_one_raw(stmt)
        .await
        .map_err(dberr_to_sqlx)?
        .ok_or_else(|| sqlx::Error::Protocol("fts probe returned no rows".to_owned()))?;
    let count: i64 = row.try_get("", "c").map_err(dberr_to_sqlx)?;
    Ok(count > 0)
}

/// Search messages for a user, optionally scoped to account/folder. Returns message ids in rank order.
pub async fn search_message_ids(
    db: &DbPool,
    user_id: &str,
    query: &str,
    account_id: Option<&str>,
    folder_id: Option<&str>,
    limit: i64,
) -> Result<Vec<String>, SearchError> {
    let user = id_value(db, user_id)?;
    let account = opt_id_value(db, account_id)?;
    let folder = opt_id_value(db, folder_id)?;
    let limit = limit.clamp(1, 500);

    let hits = match db.backend() {
        DbBackend::Sqlite => {
            sqlite_search_message_ids(db, query, &user, &account, &folder, limit).await?
        }
        #[cfg(feature = "postgres")]
        DbBackend::Postgres => {
            postgres_search_message_ids(db, query, &user, &account, &folder, limit).await?
        }
        other => {
            return Err(SearchError::from(sqlx::Error::Protocol(format!(
                "unsupported backend {other:?}"
            ))));
        }
    };
    Ok(hits.into_iter().map(|hit| hit.message_id).collect())
}

#[allow(clippy::ref_option)]
async fn sqlite_search_message_ids(
    db: &DbPool,
    query: &str,
    user: &Value,
    account: &Option<Value>,
    folder: &Option<Value>,
    limit: i64,
) -> Result<Vec<SearchHit>, SearchError> {
    let fts_query = prepare_fts5_query(query).ok_or(SearchError::InvalidQuery)?;
    let stmt = Statement::from_sql_and_values(
        DbBackend::Sqlite,
        r"
        SELECT m.id AS message_id, bm25(message_fts) AS rank
        FROM message_fts
        JOIN message m ON m.id = message_fts.message_id
        JOIN mail_account a ON m.account_id = a.id
        WHERE message_fts MATCH ?
          AND a.user_id = ?
          AND m.is_deleted = 0
          AND (? IS NULL OR m.account_id = ?)
          AND (? IS NULL OR m.folder_id = ?)
        ORDER BY rank
        LIMIT ?
        ",
        [
            Value::from(fts_query.as_str()),
            user.clone(),
            value_or_null(account.as_ref()),
            value_or_null(account.as_ref()),
            value_or_null(folder.as_ref()),
            value_or_null(folder.as_ref()),
            Value::from(limit),
        ],
    );
    let rows = db.orm().query_all_raw(stmt).await.map_err(orm_err)?;
    rows.iter().map(hit_from_row).collect()
}

#[cfg(feature = "postgres")]
#[allow(clippy::ref_option)]
async fn postgres_search_message_ids(
    db: &DbPool,
    query: &str,
    user: &Value,
    account: &Option<Value>,
    folder: &Option<Value>,
    limit: i64,
) -> Result<Vec<SearchHit>, SearchError> {
    let stmt = Statement::from_sql_and_values(
        DbBackend::Postgres,
        r"
        SELECT m.id AS message_id,
               ts_rank(m.search_vector, plainto_tsquery('simple', $1)) AS rank
        FROM message m
        JOIN mail_account a ON m.account_id = a.id
        WHERE a.user_id = $2::uuid
          AND m.is_deleted = FALSE
          AND m.search_vector @@ plainto_tsquery('simple', $1)
          AND ($3 IS NULL OR m.account_id = $4::uuid)
          AND ($5 IS NULL OR m.folder_id = $6::uuid)
        ORDER BY rank DESC
        LIMIT $7
        ",
        [
            Value::from(query),
            user.clone(),
            value_or_null(account.as_ref()),
            value_or_null(account.as_ref()),
            value_or_null(folder.as_ref()),
            value_or_null(folder.as_ref()),
            Value::from(limit),
        ],
    );
    let rows = db.orm().query_all_raw(stmt).await.map_err(orm_err)?;
    rows.iter().map(hit_from_row).collect()
}

struct SqliteSearchIndex {
    db: DbPool,
}

struct PostgresSearchIndex {
    db: DbPool,
}

#[async_trait]
impl SearchIndex for SqliteSearchIndex {
    async fn index_message(&self, msg: &MessageSearchDoc) -> Result<(), SearchError> {
        let id = id_value(&self.db, &msg.id)?;
        let account = id_value(&self.db, &msg.account_id)?;
        let conn = self.db.orm();
        conn.execute_raw(Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "DELETE FROM message_fts WHERE message_id = ?",
            [id.clone()],
        ))
        .await
        .map_err(orm_err)?;
        conn.execute_raw(Statement::from_sql_and_values(
            DbBackend::Sqlite,
            r"
            INSERT INTO message_fts (message_id, account_id, subject, body_text, from_address)
            VALUES (?, ?, ?, ?, ?)
            ",
            [
                id,
                account,
                Value::from(msg.subject.as_deref().unwrap_or("")),
                Value::from(msg.body_text.as_deref().unwrap_or("")),
                Value::from(msg.from_address.as_deref().unwrap_or("")),
            ],
        ))
        .await
        .map_err(orm_err)?;
        Ok(())
    }

    async fn remove_message(&self, id: &str) -> Result<(), SearchError> {
        let id = id_value(&self.db, id)?;
        self.db
            .orm()
            .execute_raw(Statement::from_sql_and_values(
                DbBackend::Sqlite,
                "DELETE FROM message_fts WHERE message_id = ?",
                [id],
            ))
            .await
            .map_err(orm_err)?;
        Ok(())
    }

    async fn search(
        &self,
        query: &str,
        account_id: &str,
        limit: usize,
    ) -> Result<Vec<SearchHit>, SearchError> {
        let fts_query = prepare_fts5_query(query).ok_or(SearchError::InvalidQuery)?;
        let account = if account_id.is_empty() {
            None
        } else {
            Some(id_value(&self.db, account_id)?)
        };
        let limit = i64::try_from(limit).unwrap_or(500);

        // The FTS5 MATCH text and bm25 ranking stay verbatim; only the
        // optional account scope branches.
        let rows = match &account {
            Some(account) => self
                .db
                .orm()
                .query_all_raw(Statement::from_sql_and_values(
                    DbBackend::Sqlite,
                    r"
                    SELECT message_id, bm25(message_fts) AS rank
                    FROM message_fts
                    WHERE message_fts MATCH ?
                      AND account_id = ?
                    ORDER BY rank
                    LIMIT ?
                    ",
                    [
                        Value::from(fts_query.as_str()),
                        account.clone(),
                        Value::from(limit),
                    ],
                ))
                .await
                .map_err(orm_err)?,
            None => self
                .db
                .orm()
                .query_all_raw(Statement::from_sql_and_values(
                    DbBackend::Sqlite,
                    r"
                    SELECT message_id, bm25(message_fts) AS rank
                    FROM message_fts
                    WHERE message_fts MATCH ?
                    ORDER BY rank
                    LIMIT ?
                    ",
                    [Value::from(fts_query.as_str()), Value::from(limit)],
                ))
                .await
                .map_err(orm_err)?,
        };
        rows.iter().map(hit_from_row).collect()
    }
}

#[cfg(feature = "postgres")]
#[async_trait]
impl SearchIndex for PostgresSearchIndex {
    async fn index_message(&self, msg: &MessageSearchDoc) -> Result<(), SearchError> {
        let id = id_value(&self.db, &msg.id)?;
        self.db
            .orm()
            .execute_raw(Statement::from_sql_and_values(
                DbBackend::Postgres,
                r"
                UPDATE message
                SET subject = COALESCE($1, subject),
                    body_text = COALESCE($2, body_text),
                    from_address = COALESCE($3::jsonb, from_address),
                    updated_at = NOW()
                WHERE id = $4
                ",
                [
                    Value::from(msg.subject.as_deref()),
                    Value::from(msg.body_text.as_deref()),
                    Value::from(msg.from_address.as_deref()),
                    id,
                ],
            ))
            .await
            .map_err(orm_err)?;
        Ok(())
    }

    async fn remove_message(&self, id: &str) -> Result<(), SearchError> {
        let id = id_value(&self.db, id)?;
        self.db
            .orm()
            .execute_raw(Statement::from_sql_and_values(
                DbBackend::Postgres,
                r"
                UPDATE message
                SET search_vector = NULL, updated_at = NOW()
                WHERE id = $1
                ",
                [id],
            ))
            .await
            .map_err(orm_err)?;
        Ok(())
    }

    async fn search(
        &self,
        query: &str,
        account_id: &str,
        limit: usize,
    ) -> Result<Vec<SearchHit>, SearchError> {
        let limit = i64::try_from(limit).unwrap_or(500);
        let account = if account_id.is_empty() {
            None
        } else {
            Some(id_value(&self.db, account_id)?)
        };

        // tsvector @@ plainto_tsquery / ts_rank stay verbatim.
        let rows = match &account {
            Some(account) => self
                .db
                .orm()
                .query_all_raw(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    r"
                    SELECT id AS message_id,
                           ts_rank(search_vector, plainto_tsquery('simple', $1)) AS rank
                    FROM message
                    WHERE is_deleted = FALSE
                      AND search_vector @@ plainto_tsquery('simple', $1)
                      AND account_id = $2
                    ORDER BY rank DESC
                    LIMIT $3
                    ",
                    [Value::from(query), account.clone(), Value::from(limit)],
                ))
                .await
                .map_err(orm_err)?,
            None => self
                .db
                .orm()
                .query_all_raw(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    r"
                    SELECT id AS message_id,
                           ts_rank(search_vector, plainto_tsquery('simple', $1)) AS rank
                    FROM message
                    WHERE is_deleted = FALSE
                      AND search_vector @@ plainto_tsquery('simple', $1)
                    ORDER BY rank DESC
                    LIMIT $2
                    ",
                    [Value::from(query), Value::from(limit)],
                ))
                .await
                .map_err(orm_err)?,
        };
        rows.iter().map(hit_from_row).collect()
    }
}

/// Escape a user query for FTS5 `MATCH` (token AND, double-quote escaped).
#[must_use]
pub fn prepare_fts5_query(user_query: &str) -> Option<String> {
    let tokens: Vec<String> = user_query
        .split_whitespace()
        .filter(|t| !t.is_empty())
        .map(|token| {
            let escaped = token.replace('"', "\"\"");
            format!("\"{escaped}\"")
        })
        .collect();
    if tokens.is_empty() {
        None
    } else {
        Some(tokens.join(" AND "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::Storage;
    use uuid::Uuid;

    async fn test_db() -> DbPool {
        let storage = Storage::new("sqlite::memory:").await.unwrap();
        storage.run_migrations().await.unwrap();
        storage.pool().clone()
    }

    async fn seed_message(
        db: &DbPool,
        user_id: &str,
        account_id: &str,
        folder_id: &str,
        subject: &str,
        body: &str,
        from_json: &str,
    ) -> String {
        let pool = match db {
            DbPool::Sqlite(pool) => pool,
            #[cfg(feature = "postgres")]
            DbPool::Postgres(_) => panic!("seed_message test requires sqlite pool"),
        };
        crate::auth::install_test_master_key();
        let dek = crate::crypto::generate_key();
        let kek = crate::crypto::derive_user_kek(crate::auth::TEST_MASTER_KEY, user_id);
        let wrapped_dek = crate::crypto::wrap_dek(&kek, &dek).unwrap();
        sqlx::query(
            "INSERT OR IGNORE INTO lyra_user (id, username, password_hash, encrypted_dek) VALUES (?, ?, ?, ?)",
        )
        .bind(user_id)
        .bind(format!("user-{user_id}"))
        .bind("hash")
        .bind(&wrapped_dek)
        .execute(pool)
        .await
        .unwrap();

        sqlx::query(
            r"
            INSERT OR IGNORE INTO mail_account (
                id, user_id, display_name, email_address, protocol, auth_type, credential
            ) VALUES (?, ?, 'Test', 'test@example.com', 'imap', 'password', '{}')
            ",
        )
        .bind(account_id)
        .bind(user_id)
        .execute(pool)
        .await
        .unwrap();

        sqlx::query(
            "INSERT OR IGNORE INTO folder (id, account_id, external_id, name) VALUES (?, ?, 'inbox', 'Inbox')",
        )
        .bind(folder_id)
        .bind(account_id)
        .execute(pool)
        .await
        .unwrap();

        let message_id = Uuid::new_v7(uuid::Timestamp::now(uuid::NoContext)).to_string();
        sqlx::query(
            r"
            INSERT INTO message (
                id, account_id, folder_id, external_id, subject, body_text, from_address
            ) VALUES (?, ?, ?, '1', ?, ?, ?)
            ",
        )
        .bind(&message_id)
        .bind(account_id)
        .bind(folder_id)
        .bind(subject)
        .bind(body)
        .bind(from_json)
        .execute(pool)
        .await
        .unwrap();
        message_id
    }

    #[test]
    fn prepare_fts5_query_quotes_tokens() {
        assert_eq!(
            prepare_fts5_query("hello world").as_deref(),
            Some("\"hello\" AND \"world\"")
        );
        assert_eq!(
            prepare_fts5_query(r#""quoted""#).as_deref(),
            Some("\"\"\"quoted\"\"\"")
        );
        assert!(prepare_fts5_query("   ").is_none());
    }

    #[tokio::test]
    async fn fts_available_after_migration() {
        let db = test_db().await;
        assert!(fts_available(&db).await.unwrap());
    }

    #[tokio::test]
    async fn sqlite_fts_finds_indexed_message() {
        let db = test_db().await;
        let user_id = Uuid::new_v7(uuid::Timestamp::now(uuid::NoContext)).to_string();
        let account_id = Uuid::new_v7(uuid::Timestamp::now(uuid::NoContext)).to_string();
        let folder_id = Uuid::new_v7(uuid::Timestamp::now(uuid::NoContext)).to_string();
        seed_message(
            &db,
            &user_id,
            &account_id,
            &folder_id,
            "Quarterly report",
            "Revenue grew in Q3",
            r#"{"email":"finance@example.com"}"#,
        )
        .await;

        let index = search_index_for(&db);
        let hits = index.search("quarterly", &account_id, 10).await.unwrap();
        assert_eq!(hits.len(), 1);

        let ids = search_message_ids(&db, &user_id, "revenue", Some(&account_id), None, 10)
            .await
            .unwrap();
        assert_eq!(ids.len(), 1);
    }
}

#[cfg(test)]
#[cfg(feature = "postgres")]
mod postgres_live {
    //! FTS roundtrip on PostgreSQL: the migration trigger maintains
    //! `search_vector` on insert, and the PG search branch (websearch
    //! syntax + uuid binds) must find the row.

    use crate::pgtest::support;
    use crate::sync::store;

    #[test]
    #[ignore = "needs postgres"]
    fn search_finds_seeded_message() {
        support::rt().block_on(async {
            let (db, user_id) = support::setup().await;
            let account_id = support::seed_account(&db, &user_id, "search@example.com").await;
            let folder_id = support::seed_inbox(&db, &account_id).await;
            let msg = support::message(21, "Xylophone dialect audit", "s@example.com");
            let external_id = crate::sync::store::imap_message_external_id(&folder_id, msg.uid);
            store::upsert_message(&db, &account_id, &folder_id, &msg)
                .await
                .unwrap();

            assert!(super::fts_available(&db).await.unwrap());

            let hits = super::search_message_ids(&db, &user_id, "xylophone", None, None, 10)
                .await
                .unwrap();
            let expected: Option<String> = sqlx_match_id(&db, &account_id, &external_id).await;
            assert!(
                expected.as_ref().is_some_and(|want| hits.contains(want)),
                "search must find the seeded message, got {hits:?}"
            );
        });
    }

    async fn sqlx_match_id(
        db: &crate::storage::DbPool,
        account_id: &str,
        external_id: &str,
    ) -> Option<String> {
        let crate::storage::DbPool::Postgres(pool) = db else {
            panic!("expected postgres pool")
        };
        sqlx::query_scalar::<_, String>(
            "SELECT id::text FROM message WHERE account_id = $1::uuid AND external_id = $2",
        )
        .bind(account_id)
        .bind(external_id)
        .fetch_one(pool)
        .await
        .ok()
    }
}
