//! SQLite → PostgreSQL SQL dialect helpers.
//!
//! Handlers keep one SQLite-flavoured query string (`?`, `datetime('now')`,
//! `IFNULL`, `ON CONFLICT(`). The Postgres arm runs [`to_postgres`] on that
//! string. This is not an ORM — just placeholder / function rewriting.

/// Rewrite a SQLite-style SQL string for PostgreSQL.
///
/// - `?` placeholders become `$1`, `$2`, … (skipped inside `'…'` literals)
/// - `datetime('now')` / `datetime("now")` become `NOW()`
/// - `IFNULL(` becomes `COALESCE(`
/// - `ON CONFLICT(` becomes `ON CONFLICT (`
#[must_use]
#[cfg_attr(not(feature = "postgres"), allow(dead_code))]
pub fn to_postgres(sql: &str) -> String {
    let sql = sql.replace("datetime('now')", "NOW()");
    let sql = sql.replace("datetime(\"now\")", "NOW()");
    let sql = sql.replace("IFNULL(", "COALESCE(");
    let sql = sql.replace("ifnull(", "COALESCE(");
    let sql = sql.replace("ON CONFLICT(", "ON CONFLICT (");

    let mut out = String::with_capacity(sql.len() + 8);
    let mut chars = sql.chars().peekable();
    let mut n = 0u32;
    let mut in_single = false;

    while let Some(ch) = chars.next() {
        if ch == '\'' {
            out.push(ch);
            if in_single {
                if chars.peek() == Some(&'\'') {
                    out.push(chars.next().unwrap());
                } else {
                    in_single = false;
                }
            } else {
                in_single = true;
            }
            continue;
        }
        if !in_single && ch == '?' {
            n += 1;
            out.push('$');
            out.push_str(&n.to_string());
            continue;
        }
        out.push(ch);
    }
    out
}

/// Execute a statement against either pool. Returns `rows_affected`.
#[macro_export]
macro_rules! db_execute {
    ($db:expr, $sql:expr $(, $bind:expr)* $(,)?) => {{
        match $db {
            $crate::storage::DbPool::Sqlite(pool) => {
                sqlx::query($sql)
                    $(.bind($bind))*
                    .execute(pool)
                    .await
                    .map(|r| r.rows_affected())
            }
            #[cfg(feature = "postgres")]
            $crate::storage::DbPool::Postgres(pool) => {
                let __sql = $crate::db_sql::to_postgres($sql);
                sqlx::query(&__sql)
                    $(.bind($bind))*
                    .execute(pool)
                    .await
                    .map(|r| r.rows_affected())
            }
        }
    }};
}

/// `fetch_all` + map each row to a shared type.
#[macro_export]
macro_rules! db_fetch_all {
    ($db:expr, $sql:expr, |$row:ident| $map:expr $(, $bind:expr)* $(,)?) => {{
        match $db {
            $crate::storage::DbPool::Sqlite(pool) => {
                sqlx::query($sql)
                    $(.bind($bind))*
                    .fetch_all(pool)
                    .await
                    .map(|rows| rows.iter().map(|$row| $map).collect::<Vec<_>>())
            }
            #[cfg(feature = "postgres")]
            $crate::storage::DbPool::Postgres(pool) => {
                let __sql = $crate::db_sql::to_postgres($sql);
                sqlx::query(&__sql)
                    $(.bind($bind))*
                    .fetch_all(pool)
                    .await
                    .map(|rows| rows.iter().map(|$row| $map).collect::<Vec<_>>())
            }
        }
    }};
}

/// `fetch_optional` + map the row to a shared type.
#[macro_export]
macro_rules! db_fetch_optional {
    ($db:expr, $sql:expr, |$row:ident| $map:expr $(, $bind:expr)* $(,)?) => {{
        match $db {
            $crate::storage::DbPool::Sqlite(pool) => {
                sqlx::query($sql)
                    $(.bind($bind))*
                    .fetch_optional(pool)
                    .await
                    .map(|opt| opt.map(|$row| $map))
            }
            #[cfg(feature = "postgres")]
            $crate::storage::DbPool::Postgres(pool) => {
                let __sql = $crate::db_sql::to_postgres($sql);
                sqlx::query(&__sql)
                    $(.bind($bind))*
                    .fetch_optional(pool)
                    .await
                    .map(|opt| opt.map(|$row| $map))
            }
        }
    }};
}

/// `fetch_one` + map the row to a shared type.
#[macro_export]
macro_rules! db_fetch_one {
    ($db:expr, $sql:expr, |$row:ident| $map:expr $(, $bind:expr)* $(,)?) => {{
        match $db {
            $crate::storage::DbPool::Sqlite(pool) => {
                sqlx::query($sql)
                    $(.bind($bind))*
                    .fetch_one(pool)
                    .await
                    .map(|$row| $map)
            }
            #[cfg(feature = "postgres")]
            $crate::storage::DbPool::Postgres(pool) => {
                let __sql = $crate::db_sql::to_postgres($sql);
                sqlx::query(&__sql)
                    $(.bind($bind))*
                    .fetch_one(pool)
                    .await
                    .map(|$row| $map)
            }
        }
    }};
}

/// `query_scalar` + `fetch_optional`.
#[macro_export]
macro_rules! db_scalar_optional {
    ($db:expr, $ty:ty, $sql:expr $(, $bind:expr)* $(,)?) => {{
        match $db {
            $crate::storage::DbPool::Sqlite(pool) => {
                sqlx::query_scalar::<_, $ty>($sql)
                    $(.bind($bind))*
                    .fetch_optional(pool)
                    .await
            }
            #[cfg(feature = "postgres")]
            $crate::storage::DbPool::Postgres(pool) => {
                let __sql = $crate::db_sql::to_postgres($sql);
                sqlx::query_scalar::<_, $ty>(&__sql)
                    $(.bind($bind))*
                    .fetch_optional(pool)
                    .await
            }
        }
    }};
}

/// `query_scalar` + `fetch_one`.
#[macro_export]
macro_rules! db_scalar {
    ($db:expr, $ty:ty, $sql:expr $(, $bind:expr)* $(,)?) => {{
        match $db {
            $crate::storage::DbPool::Sqlite(pool) => {
                sqlx::query_scalar::<_, $ty>($sql)
                    $(.bind($bind))*
                    .fetch_one(pool)
                    .await
            }
            #[cfg(feature = "postgres")]
            $crate::storage::DbPool::Postgres(pool) => {
                let __sql = $crate::db_sql::to_postgres($sql);
                sqlx::query_scalar::<_, $ty>(&__sql)
                    $(.bind($bind))*
                    .fetch_one(pool)
                    .await
            }
        }
    }};
}

/// Bind a slice of values onto a SQLite or Postgres query.
#[macro_export]
macro_rules! db_execute_binds {
    ($db:expr, $sql:expr, $binds:expr) => {{
        match $db {
            $crate::storage::DbPool::Sqlite(pool) => {
                let mut query = sqlx::query($sql);
                for value in $binds {
                    query = query.bind(value);
                }
                query.execute(pool).await.map(|r| r.rows_affected())
            }
            #[cfg(feature = "postgres")]
            $crate::storage::DbPool::Postgres(pool) => {
                let __sql = $crate::db_sql::to_postgres($sql);
                let mut query = sqlx::query(&__sql);
                for value in $binds {
                    query = query.bind(value);
                }
                query.execute(pool).await.map(|r| r.rows_affected())
            }
        }
    }};
}

#[cfg(test)]
mod tests {
    use super::to_postgres;

    #[test]
    fn numbers_question_placeholders() {
        assert_eq!(
            to_postgres("SELECT id FROM t WHERE a = ? AND b = ?"),
            "SELECT id FROM t WHERE a = $1 AND b = $2"
        );
    }

    #[test]
    fn leaves_question_marks_inside_strings() {
        assert_eq!(
            to_postgres("SELECT '?' FROM t WHERE id = ?"),
            "SELECT '?' FROM t WHERE id = $1"
        );
        assert_eq!(
            to_postgres("SELECT 'it''s ?' FROM t WHERE id = ?"),
            "SELECT 'it''s ?' FROM t WHERE id = $1"
        );
    }

    #[test]
    fn rewrites_datetime_now_and_ifnull() {
        assert_eq!(
            to_postgres("UPDATE t SET ts = datetime('now') WHERE x = IFNULL(y, ?)"),
            "UPDATE t SET ts = NOW() WHERE x = COALESCE(y, $1)"
        );
        assert_eq!(
            to_postgres(r#"UPDATE t SET ts = datetime("now")"#),
            "UPDATE t SET ts = NOW()"
        );
    }

    #[test]
    fn spaces_on_conflict_target() {
        assert_eq!(
            to_postgres(
                "INSERT INTO t (id) VALUES (?) ON CONFLICT(id) DO UPDATE SET id = excluded.id"
            ),
            "INSERT INTO t (id) VALUES ($1) ON CONFLICT (id) DO UPDATE SET id = excluded.id"
        );
    }
}
