//! SQLite → PostgreSQL SQL dialect helpers.
//!
//! Handlers keep one SQLite-flavoured query string (`?`, `datetime('now')`,
//! `IFNULL`, `ON CONFLICT(`). The Postgres arm runs [`to_postgres`] on that
//! string. This is not an ORM — just placeholder / function rewriting.

/// Integer-flag columns that SQLite stores as 0/1 and Postgres stores as BOOLEAN.
const SQLITE_BOOL_FLAGS: &[&str] = &[
    "is_active",
    "sync_enabled",
    "totp_enabled",
    "is_starred",
    "is_read",
    "is_draft",
    "is_deleted",
    "has_attachments",
    "is_inline",
    "is_all_day",
];

/// Rewrite a SQLite-style SQL string for PostgreSQL.
///
/// - `?` placeholders become `$1`, `$2`, … (skipped inside `'…'` literals)
/// - `datetime('now')` / `datetime("now")` become `NOW()`
/// - `IFNULL(` becomes `COALESCE(`
/// - `ON CONFLICT(` becomes `ON CONFLICT (`
/// - known integer-flag predicates (`is_active = 1`) become `TRUE`/`FALSE`
/// - `LIKE` becomes `ILIKE` (skipped inside `'…'` literals and existing `ILIKE`)
#[must_use]
#[cfg_attr(not(feature = "postgres"), allow(dead_code))]
pub fn to_postgres(sql: &str) -> String {
    let sql = sql.replace("datetime('now')", "NOW()");
    let sql = sql.replace("datetime(\"now\")", "NOW()");
    let sql = sql.replace("IFNULL(", "COALESCE(");
    let sql = sql.replace("ifnull(", "COALESCE(");
    let sql = sql.replace("ON CONFLICT(", "ON CONFLICT (");
    let sql = rewrite_bool_flag_literals(&sql);
    let sql = rewrite_like_to_ilike(&sql);

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

fn rewrite_bool_flag_literals(sql: &str) -> String {
    let bytes = sql.as_bytes();
    let mut out = String::with_capacity(sql.len() + 16);
    let mut i = 0;
    let mut in_single = false;

    while i < bytes.len() {
        if bytes[i] == b'\'' {
            out.push('\'');
            if in_single {
                if bytes.get(i + 1) == Some(&b'\'') {
                    out.push('\'');
                    i += 2;
                    continue;
                }
                in_single = false;
            } else {
                in_single = true;
            }
            i += 1;
            continue;
        }
        if !in_single && let Some((consumed, replacement)) = match_bool_flag_literal(sql, i) {
            out.push_str(&replacement);
            i += consumed;
            continue;
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

fn match_bool_flag_literal(sql: &str, i: usize) -> Option<(usize, String)> {
    if i > 0 {
        let prev = sql.as_bytes()[i - 1];
        if prev.is_ascii_alphanumeric() || prev == b'_' {
            return None;
        }
    }
    let rest = sql.get(i..)?;
    let ident_end = rest
        .find(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_' && ch != '.')
        .unwrap_or(rest.len());
    if ident_end == 0 {
        return None;
    }
    let ident = &rest[..ident_end];
    let col = ident.rsplit('.').next()?;
    if !SQLITE_BOOL_FLAGS.contains(&col) {
        return None;
    }
    let after = rest.get(ident_end..)?;
    let trimmed = after.trim_start();
    let skipped = after.len() - trimmed.len();
    let (value, value_len) = if trimmed.starts_with("= 1") {
        ("TRUE", 3usize)
    } else if trimmed.starts_with("= 0") {
        ("FALSE", 3)
    } else if trimmed.starts_with("=1") {
        ("TRUE", 2)
    } else if trimmed.starts_with("=0") {
        ("FALSE", 2)
    } else {
        return None;
    };
    let after_value = ident_end + skipped + value_len;
    if rest
        .get(after_value..)
        .and_then(|s| s.chars().next())
        .is_some_and(|ch| ch.is_ascii_alphanumeric() || ch == '_')
    {
        return None;
    }
    Some((after_value, format!("{ident} = {value}")))
}

fn rewrite_like_to_ilike(sql: &str) -> String {
    let bytes = sql.as_bytes();
    let mut out = String::with_capacity(sql.len() + 8);
    let mut i = 0;
    let mut in_single = false;

    while i < bytes.len() {
        if bytes[i] == b'\'' {
            out.push('\'');
            if in_single {
                if bytes.get(i + 1) == Some(&b'\'') {
                    out.push('\'');
                    i += 2;
                    continue;
                }
                in_single = false;
            } else {
                in_single = true;
            }
            i += 1;
            continue;
        }
        if !in_single && like_keyword_at(sql, i) {
            out.push_str("ILIKE");
            i += 4;
            continue;
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

fn like_keyword_at(sql: &str, i: usize) -> bool {
    let rest = sql.get(i..).unwrap_or("");
    if rest.len() < 4 || !rest[..4].eq_ignore_ascii_case("like") {
        return false;
    }
    if rest.len() > 4 {
        let next = rest.as_bytes()[4];
        if next.is_ascii_alphanumeric() || next == b'_' {
            return false;
        }
    }
    if i > 0 {
        let prev = sql.as_bytes()[i - 1];
        if prev.is_ascii_alphanumeric() || prev == b'_' {
            return false;
        }
    }
    true
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

    #[test]
    fn rewrites_sqlite_boolean_flag_literals() {
        assert_eq!(
            to_postgres("SELECT id FROM mail_account WHERE is_active = 1 AND sync_enabled = 0"),
            "SELECT id FROM mail_account WHERE is_active = TRUE AND sync_enabled = FALSE"
        );
        assert_eq!(
            to_postgres("SELECT id FROM message WHERE m.is_deleted = 0 AND has_attachments = 1"),
            "SELECT id FROM message WHERE m.is_deleted = FALSE AND has_attachments = TRUE"
        );
        // Integer columns / LIMIT must stay numeric.
        assert_eq!(
            to_postgres("SELECT id FROM jobs WHERE attempts = 1 LIMIT 1"),
            "SELECT id FROM jobs WHERE attempts = 1 LIMIT 1"
        );
    }

    #[test]
    fn rewrites_like_to_ilike_outside_strings() {
        assert_eq!(
            to_postgres("SELECT id FROM t WHERE subject LIKE ? OR snippet LIKE ?"),
            "SELECT id FROM t WHERE subject ILIKE $1 OR snippet ILIKE $2"
        );
        assert_eq!(
            to_postgres("SELECT 'looks LIKE ?' FROM t WHERE id = ?"),
            "SELECT 'looks LIKE ?' FROM t WHERE id = $1"
        );
        assert_eq!(
            to_postgres("SELECT id FROM t WHERE name ILIKE ?"),
            "SELECT id FROM t WHERE name ILIKE $1"
        );
    }
}
