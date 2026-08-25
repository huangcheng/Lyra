//! Full-text search over local message index (FTS5 / tsvector).
//!
//! See `docs/specs/2026-08-20-lyra-data-model-spec.md` §5.

#![allow(dead_code)] // seam wired in main; index/search callers land incrementally

use async_trait::async_trait;
use sqlx::Row;
use std::sync::Arc;
use thiserror::Error;

use crate::db_row::{InvalidIdError, id_param, opt_id_param};
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
    InvalidId(#[from] InvalidIdError),
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

/// Whether migration 0009 has been applied (FTS table / tsvector column present).
pub async fn fts_available(db: &DbPool) -> Result<bool, sqlx::Error> {
    match db {
        DbPool::Sqlite(pool) => {
            let count: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'message_fts'",
            )
            .fetch_one(pool)
            .await?;
            Ok(count > 0)
        }
        #[cfg(feature = "postgres")]
        DbPool::Postgres(pool) => {
            let count: i64 = sqlx::query_scalar(
                r"
                SELECT COUNT(*)
                FROM information_schema.columns
                WHERE table_schema = 'public'
                  AND table_name = 'message'
                  AND column_name = 'search_vector'
                ",
            )
            .fetch_one(pool)
            .await?;
            Ok(count > 0)
        }
    }
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
    let user_bind = id_param(db, user_id)?;
    let account_bind = opt_id_param(db, account_id)?;
    let folder_bind = opt_id_param(db, folder_id)?;
    let limit = limit.clamp(1, 500);

    match db {
        DbPool::Sqlite(_) => {
            sqlite_search_message_ids(
                db,
                query,
                &user_bind,
                &account_bind,
                &account_bind,
                &folder_bind,
                &folder_bind,
                limit,
            )
            .await
        }
        #[cfg(feature = "postgres")]
        DbPool::Postgres(_) => {
            postgres_search_message_ids(
                db,
                query,
                &user_bind,
                &account_bind,
                &account_bind,
                &folder_bind,
                &folder_bind,
                limit,
            )
            .await
        }
    }
}

#[allow(clippy::too_many_arguments, clippy::ref_option)]
async fn sqlite_search_message_ids(
    db: &DbPool,
    query: &str,
    user_bind: &crate::db_row::IdParam,
    account_bind: &Option<crate::db_row::IdParam>,
    account_bind_dup: &Option<crate::db_row::IdParam>,
    folder_bind: &Option<crate::db_row::IdParam>,
    folder_bind_dup: &Option<crate::db_row::IdParam>,
    limit: i64,
) -> Result<Vec<String>, SearchError> {
    let fts_query = prepare_fts5_query(query).ok_or(SearchError::InvalidQuery)?;
    let rows = db_fetch_all!(
        db,
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
        |row| SearchHit {
            message_id: row.get::<String, _>("message_id"),
            rank: row.get::<f64, _>("rank"),
        },
        &fts_query,
        user_bind,
        account_bind,
        account_bind_dup,
        folder_bind,
        folder_bind_dup,
        limit
    )?;
    Ok(rows.into_iter().map(|hit| hit.message_id).collect())
}

#[cfg(feature = "postgres")]
#[allow(clippy::too_many_arguments, clippy::ref_option)]
async fn postgres_search_message_ids(
    db: &DbPool,
    query: &str,
    user_bind: &crate::db_row::IdParam,
    account_bind: &Option<crate::db_row::IdParam>,
    account_bind_dup: &Option<crate::db_row::IdParam>,
    folder_bind: &Option<crate::db_row::IdParam>,
    folder_bind_dup: &Option<crate::db_row::IdParam>,
    limit: i64,
) -> Result<Vec<String>, SearchError> {
    let rows = db_fetch_all!(
        db,
        r"
        SELECT m.id AS message_id,
               ts_rank(m.search_vector, plainto_tsquery('simple', ?)) AS rank
        FROM message m
        JOIN mail_account a ON m.account_id = a.id
        WHERE a.user_id = ?
          AND m.is_deleted = 0
          AND m.search_vector @@ plainto_tsquery('simple', ?)
          AND (? IS NULL OR m.account_id = ?)
          AND (? IS NULL OR m.folder_id = ?)
        ORDER BY rank DESC
        LIMIT ?
        ",
        |row| SearchHit {
            message_id: crate::db_row::id_from_row(row, "message_id"),
            rank: row.get::<f64, _>("rank"),
        },
        query,
        user_bind,
        query,
        account_bind,
        account_bind_dup,
        folder_bind,
        folder_bind_dup,
        limit
    )?;
    Ok(rows.into_iter().map(|hit| hit.message_id).collect())
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
        let id_bind = id_param(&self.db, &msg.id)?;
        let account_bind = id_param(&self.db, &msg.account_id)?;
        db_execute!(
            &self.db,
            "DELETE FROM message_fts WHERE message_id = ?",
            &id_bind
        )?;
        db_execute!(
            &self.db,
            r"
            INSERT INTO message_fts (message_id, account_id, subject, body_text, from_address)
            VALUES (?, ?, ?, ?, ?)
            ",
            &id_bind,
            &account_bind,
            msg.subject.as_deref().unwrap_or(""),
            msg.body_text.as_deref().unwrap_or(""),
            msg.from_address.as_deref().unwrap_or("")
        )?;
        Ok(())
    }

    async fn remove_message(&self, id: &str) -> Result<(), SearchError> {
        let id_bind = id_param(&self.db, id)?;
        db_execute!(
            &self.db,
            "DELETE FROM message_fts WHERE message_id = ?",
            &id_bind
        )?;
        Ok(())
    }

    async fn search(
        &self,
        query: &str,
        account_id: &str,
        limit: usize,
    ) -> Result<Vec<SearchHit>, SearchError> {
        let fts_query = prepare_fts5_query(query).ok_or(SearchError::InvalidQuery)?;
        let account_bind = if account_id.is_empty() {
            None
        } else {
            Some(id_param(&self.db, account_id)?)
        };
        let limit = i64::try_from(limit).unwrap_or(500);

        let rows = match &account_bind {
            Some(account) => db_fetch_all!(
                &self.db,
                r"
                    SELECT message_id, bm25(message_fts) AS rank
                    FROM message_fts
                    WHERE message_fts MATCH ?
                      AND account_id = ?
                    ORDER BY rank
                    LIMIT ?
                    ",
                |row| SearchHit {
                    message_id: row.get::<String, _>("message_id"),
                    rank: row.get::<f64, _>("rank"),
                },
                &fts_query,
                account,
                limit
            )?,
            None => db_fetch_all!(
                &self.db,
                r"
                SELECT message_id, bm25(message_fts) AS rank
                FROM message_fts
                WHERE message_fts MATCH ?
                ORDER BY rank
                LIMIT ?
                ",
                |row| SearchHit {
                    message_id: row.get::<String, _>("message_id"),
                    rank: row.get::<f64, _>("rank"),
                },
                &fts_query,
                limit
            )?,
        };
        Ok(rows)
    }
}

#[cfg(feature = "postgres")]
#[async_trait]
impl SearchIndex for PostgresSearchIndex {
    async fn index_message(&self, msg: &MessageSearchDoc) -> Result<(), SearchError> {
        let id_bind = id_param(&self.db, &msg.id)?;
        db_execute!(
            &self.db,
            r"
            UPDATE message
            SET subject = COALESCE(?, subject),
                body_text = COALESCE(?, body_text),
                from_address = COALESCE(?::jsonb, from_address),
                updated_at = datetime('now')
            WHERE id = ?
            ",
            msg.subject.as_deref(),
            msg.body_text.as_deref(),
            msg.from_address.as_deref(),
            &id_bind
        )?;
        Ok(())
    }

    async fn remove_message(&self, id: &str) -> Result<(), SearchError> {
        let id_bind = id_param(&self.db, id)?;
        db_execute!(
            &self.db,
            r"
            UPDATE message
            SET search_vector = NULL, updated_at = datetime('now')
            WHERE id = ?
            ",
            &id_bind
        )?;
        Ok(())
    }

    async fn search(
        &self,
        query: &str,
        account_id: &str,
        limit: usize,
    ) -> Result<Vec<SearchHit>, SearchError> {
        let limit = i64::try_from(limit).unwrap_or(500);
        let account_bind = if account_id.is_empty() {
            None
        } else {
            Some(id_param(&self.db, account_id)?)
        };

        let rows = match &account_bind {
            Some(account) => db_fetch_all!(
                &self.db,
                r"
                SELECT id AS message_id,
                       ts_rank(search_vector, plainto_tsquery('simple', ?)) AS rank
                FROM message
                WHERE is_deleted = 0
                  AND search_vector @@ plainto_tsquery('simple', ?)
                  AND account_id = ?
                ORDER BY rank DESC
                LIMIT ?
                ",
                |row| SearchHit {
                    message_id: crate::db_row::id_from_row(row, "message_id"),
                    rank: row.get::<f64, _>("rank"),
                },
                query,
                query,
                account,
                limit
            )?,
            None => db_fetch_all!(
                &self.db,
                r"
                SELECT id AS message_id,
                       ts_rank(search_vector, plainto_tsquery('simple', ?)) AS rank
                FROM message
                WHERE is_deleted = 0
                  AND search_vector @@ plainto_tsquery('simple', ?)
                ORDER BY rank DESC
                LIMIT ?
                ",
                |row| SearchHit {
                    message_id: crate::db_row::id_from_row(row, "message_id"),
                    rank: row.get::<f64, _>("rank"),
                },
                query,
                query,
                limit
            )?,
        };
        Ok(rows)
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
