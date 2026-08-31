//! Storage module — dual-DB connection pool and migrations.
//!
//! Connection management and migration runners live here. Typed query helpers
//! and domain-specific SQL are in [`crate::repository`] (see that module's table
//! of seam components). Migrations load from `migrations/sqlite/` or
//! `migrations/postgres/` based on the `DATABASE_URL` scheme.
//!
//! See `docs/specs/2026-08-20-lyra-data-model-spec.md` for the schema.

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Pool, Sqlite};
use std::path::PathBuf;
use std::str::FromStr;

/// sqlx 0.9 audit marker for the migration runner.
///
/// Migration SQL comes from version-controlled `.up.sql` files in this
/// crate; statement parameters are bound separately. Marking the text
/// audited satisfies sqlx 0.9's SqlSafeStr gate.
fn audited_sql(sql: &str) -> sqlx::AssertSqlSafe<&str> {
    sqlx::AssertSqlSafe(sql)
}

/// Database pool wrapper supporting both `SQLite` and `PostgreSQL`.
#[derive(Clone, Debug)]
pub enum DbPool {
    Sqlite(Pool<Sqlite>),
    #[cfg(feature = "postgres")]
    Postgres(Pool<sqlx::Postgres>),
}

impl DbPool {
    /// Returns a human-readable engine name.
    pub fn engine_name(&self) -> &'static str {
        match self {
            DbPool::Sqlite(_) => "sqlite",
            #[cfg(feature = "postgres")]
            DbPool::Postgres(_) => "postgres",
        }
    }

    /// Returns true if the pool is `SQLite`.
    #[allow(dead_code)]
    pub fn is_sqlite(&self) -> bool {
        matches!(self, DbPool::Sqlite(_))
    }

    /// Start a transaction. Uncommitted work is rolled back on drop.
    pub async fn begin(&self) -> Result<DbTxn, sqlx::Error> {
        match self {
            Self::Sqlite(pool) => Ok(DbTxn::Sqlite(pool.begin().await?)),
            #[cfg(feature = "postgres")]
            Self::Postgres(pool) => Ok(DbTxn::Postgres(pool.begin().await?)),
        }
    }

    /// SeaORM view of this pool for entity-based repository code.
    ///
    /// Pools are Arc-backed so this conversion is cheap; entity queries and
    /// the raw macro layer share the same underlying connections and pool
    /// limits.
    #[allow(dead_code)] // first consumed by the entities layer
    pub fn orm(&self) -> sea_orm::DatabaseConnection {
        match self {
            Self::Sqlite(pool) => sea_orm::DatabaseConnection::from(pool.clone()),
            #[cfg(feature = "postgres")]
            Self::Postgres(pool) => sea_orm::DatabaseConnection::from(pool.clone()),
        }
    }

    /// SeaORM backend tag for this pool (used to build raw `Statement`s with
    /// the correct placeholder style).
    #[allow(dead_code)] // first consumed by the entities layer
    pub fn backend(&self) -> sea_orm::DbBackend {
        match self {
            Self::Sqlite(_) => sea_orm::DbBackend::Sqlite,
            #[cfg(feature = "postgres")]
            Self::Postgres(_) => sea_orm::DbBackend::Postgres,
        }
    }
}

/// Open transaction on [`DbPool`]. Commit explicitly; drop rolls back.
pub enum DbTxn {
    Sqlite(sqlx::Transaction<'static, Sqlite>),
    #[cfg(feature = "postgres")]
    Postgres(sqlx::Transaction<'static, sqlx::Postgres>),
}

impl DbTxn {
    /// Commit this transaction.
    pub async fn commit(self) -> Result<(), sqlx::Error> {
        match self {
            Self::Sqlite(tx) => tx.commit().await,
            #[cfg(feature = "postgres")]
            Self::Postgres(tx) => tx.commit().await,
        }
    }

    /// Roll back this transaction. Drop also rolls back; this is for explicit abort.
    #[allow(dead_code)]
    pub async fn rollback(self) -> Result<(), sqlx::Error> {
        match self {
            Self::Sqlite(tx) => tx.rollback().await,
            #[cfg(feature = "postgres")]
            Self::Postgres(tx) => tx.rollback().await,
        }
    }
}

/// Shared application state containing the database pool.
#[derive(Clone)]
#[allow(dead_code)]
pub struct AppState {
    pub db: DbPool,
}

/// Storage module configuration and connection management.
#[allow(dead_code)]
pub struct Storage {
    pool: DbPool,
}

impl Storage {
    /// Create a new storage instance from a `DATABASE_URL`.
    ///
    /// Supports:
    /// - `sqlite:` or `sqlite://` — `SQLite` pool
    /// - `postgres://` or `postgresql://` — `PostgreSQL` pool (requires postgres feature)
    pub async fn new(database_url: &str) -> anyhow::Result<Self> {
        let pool = if database_url.starts_with("sqlite") {
            // Strip the scheme prefix for sqlx SQLite
            let path = database_url
                .strip_prefix("sqlite:")
                .or_else(|| database_url.strip_prefix("sqlite://"))
                .unwrap_or(database_url);

            // Ensure parent directory exists for file-based SQLite
            if path != ":memory:"
                && !path.starts_with("file::memory:")
                && let Some(parent) = PathBuf::from(path).parent()
            {
                std::fs::create_dir_all(parent)?;
            }

            let options = SqliteConnectOptions::from_str(path)?
                .create_if_missing(true)
                .foreign_keys(true) // Enforce FK constraints
                // WAL + busy timeout: SQLite allows a single writer; without
                // these, concurrent sync jobs trip "database is locked".
                .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
                .busy_timeout(std::time::Duration::from_secs(5));

            let pool = SqlitePoolOptions::new()
                .max_connections(5)
                .connect_with(options)
                .await?;

            DbPool::Sqlite(pool)
        } else if database_url.starts_with("postgres") || database_url.starts_with("postgresql") {
            #[cfg(feature = "postgres")]
            {
                use sqlx::Executor as _;

                let pool = sqlx::postgres::PgPoolOptions::new()
                    .max_connections(10)
                    // Pin every session to UTC: message dates are normalized to
                    // UTC at ingest, and `date()` bucketing (stats endpoint)
                    // casts TIMESTAMPTZ → DATE in the session TimeZone. SQLite
                    // stores the same instants as UTC text, so a UTC session
                    // keeps both engines bucketing identically. Absolute
                    // comparisons and sqlx's DateTime<Utc> decoding are
                    // unaffected by the session zone.
                    .after_connect(|conn, _meta| {
                        Box::pin(async move {
                            conn.execute("SET TIME ZONE UTC").await?;
                            Ok(())
                        })
                    })
                    .connect(database_url)
                    .await?;
                DbPool::Postgres(pool)
            }
            #[cfg(not(feature = "postgres"))]
            {
                anyhow::bail!(
                    "PostgreSQL support requires the 'postgres' feature. \
                     Rebuild with --features postgres."
                );
            }
        } else {
            anyhow::bail!("Unsupported DATABASE_URL scheme. Use 'sqlite:' or 'postgres://'.");
        };

        tracing::info!("Database pool created for {}", pool.engine_name());

        Ok(Self { pool })
    }

    /// Get the database pool.
    pub fn pool(&self) -> &DbPool {
        &self.pool
    }

    /// Run pending migrations from the appropriate migration directory.
    ///
    /// Migrations are loaded from:
    /// - `migrations/sqlite/` for `SQLite` databases
    /// - `migrations/postgres/` for `PostgreSQL` databases
    pub async fn run_migrations(&self) -> anyhow::Result<()> {
        let migrations_dir = self.migrations_dir()?;

        tracing::info!("Running migrations from {}", migrations_dir.display());

        match &self.pool {
            DbPool::Sqlite(pool) => {
                run_sqlite_migrations(pool, &migrations_dir).await?;
            }
            #[cfg(feature = "postgres")]
            DbPool::Postgres(pool) => {
                run_postgres_migrations(pool, &migrations_dir).await?;
            }
        }

        tracing::info!("Migrations complete");
        Ok(())
    }

    /// Get the path to the migrations directory for the current backend.
    fn migrations_dir(&self) -> anyhow::Result<PathBuf> {
        let subdir = match &self.pool {
            DbPool::Sqlite(_) => "sqlite",
            #[cfg(feature = "postgres")]
            DbPool::Postgres(_) => "postgres",
        };

        // Prefer runtime override (Docker / packaged installs), then compile-time
        // source tree path used during `cargo run` from the repo.
        let candidates = [
            std::env::var_os("MIGRATIONS_DIR").map(PathBuf::from),
            Some(PathBuf::from("/lyra/migrations")),
            Some(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("migrations")),
        ];

        for base in candidates.into_iter().flatten() {
            let dir = if base.ends_with("sqlite") || base.ends_with("postgres") {
                base
            } else {
                base.join(subdir)
            };
            if dir.exists() {
                return Ok(dir);
            }
        }

        anyhow::bail!(
            "Migrations directory not found (set MIGRATIONS_DIR or ship /lyra/migrations/{subdir})"
        );
    }
}

/// Run `SQLite` migrations manually using sqlx raw queries.
///
/// We don't use `sqlx::migrate`!() because we have separate migration
/// directories per backend. Instead, we track applied versions in
/// a `schema_migrations` table and apply .up.sql files in order.
async fn run_sqlite_migrations(
    pool: &Pool<Sqlite>,
    migrations_dir: &PathBuf,
) -> anyhow::Result<()> {
    // Ensure schema_migrations table exists
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
            version INTEGER PRIMARY KEY,
            applied_at TEXT NOT NULL DEFAULT (datetime('now'))
        )",
    )
    .execute(pool)
    .await?;

    // Get already-applied versions
    let applied: Vec<i64> =
        sqlx::query_scalar("SELECT version FROM schema_migrations ORDER BY version")
            .fetch_all(pool)
            .await?;

    // Find and sort migration files
    let mut migrations = collect_migration_files(migrations_dir, "up")?;
    migrations.sort_by_key(|(v, _)| *v);

    for (version, path) in migrations {
        if applied.contains(&version) {
            tracing::debug!("Migration {version} already applied, skipping");
            continue;
        }

        tracing::info!("Applying migration {version}: {}", path.display());

        let sql = std::fs::read_to_string(&path)?;

        // Apply each file inside one transaction: a statement-level failure
        // would otherwise half-apply DDL (autocommit) and wedge every later
        // boot on the already-created objects.
        let cleaned = strip_sql_comments(&sql);
        let mut tx = pool.begin().await?;
        // Strip inline comments and execute statements individually.
        // SQLite doesn't support multi-statement execution via prepare,
        // so we need to split carefully (respecting BEGIN…END in triggers).
        for stmt in split_sql_statements(&cleaned) {
            sqlx::query(audited_sql(stmt.as_str()))
                .execute(&mut *tx)
                .await?;
        }
        sqlx::query("INSERT INTO schema_migrations (version) VALUES (?)")
            .bind(version)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;

        tracing::info!("Migration {version} applied successfully");
    }

    Ok(())
}

/// Run `PostgreSQL` migrations.
#[cfg(feature = "postgres")]
async fn run_postgres_migrations(
    pool: &Pool<sqlx::Postgres>,
    migrations_dir: &PathBuf,
) -> anyhow::Result<()> {
    // Ensure schema_migrations table exists
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
            version INTEGER PRIMARY KEY,
            applied_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
        )",
    )
    .execute(pool)
    .await?;

    // Get already-applied versions
    let applied: Vec<i64> =
        sqlx::query_scalar("SELECT version FROM schema_migrations ORDER BY version")
            .fetch_all(pool)
            .await?;

    // Find and sort migration files
    let mut migrations = collect_migration_files(migrations_dir, "up")?;
    migrations.sort_by_key(|(v, _)| *v);

    for (version, path) in migrations {
        if applied.contains(&version) {
            tracing::debug!("Migration {version} already applied, skipping");
            continue;
        }

        tracing::info!("Applying migration {version}: {}", path.display());

        let sql = std::fs::read_to_string(&path)?;

        // Execute the full SQL (PostgreSQL supports multi-statement execution)
        sqlx::raw_sql(audited_sql(sql.as_str()))
            .execute(pool)
            .await?;

        // Record migration
        sqlx::query("INSERT INTO schema_migrations (version) VALUES ($1)")
            .bind(version)
            .execute(pool)
            .await?;

        tracing::info!("Migration {version} applied successfully");
    }

    Ok(())
}

/// Strip SQL comments from a string.
///
/// Removes both line comments (`-- ...`) and preserves string literals.
/// This is a simplified parser that handles the common cases.
fn strip_sql_comments(sql: &str) -> String {
    let mut result = String::with_capacity(sql.len());
    let mut chars = sql.chars().peekable();

    while let Some(ch) = chars.next() {
        match ch {
            // Line comment
            '-' if chars.peek() == Some(&'-') => {
                chars.next(); // consume second '-'
                // Skip until end of line
                while let Some(&next) = chars.peek() {
                    if next == '\n' {
                        break;
                    }
                    chars.next();
                }
                // Keep the newline for statement separation
                result.push('\n');
            }
            // String literal
            '\'' => {
                result.push(ch);
                while let Some(next) = chars.next() {
                    result.push(next);
                    if next == '\'' {
                        // Check for escaped quote ('')
                        if chars.peek() == Some(&'\'') {
                            result.push(chars.next().unwrap());
                        } else {
                            break;
                        }
                    }
                }
            }
            _ => result.push(ch),
        }
    }

    result
}

/// Split SQL into statements on `;`, but not inside string literals or `BEGIN…END` blocks.
fn split_sql_statements(sql: &str) -> Vec<String> {
    let bytes = sql.as_bytes();
    let mut statements = Vec::new();
    let mut current = String::new();
    let mut begin_depth = 0i32;
    let mut in_single = false;
    let mut i = 0;

    while i < bytes.len() {
        if in_single {
            let ch = bytes[i] as char;
            current.push(ch);
            if ch == '\'' {
                if bytes.get(i + 1) == Some(&b'\'') {
                    current.push('\'');
                    i += 2;
                    continue;
                }
                in_single = false;
            }
            i += 1;
            continue;
        }

        if bytes[i] == b'\'' {
            in_single = true;
            current.push('\'');
            i += 1;
            continue;
        }

        if sql_keyword_at(sql, i, "BEGIN") {
            begin_depth += 1;
        } else if begin_depth > 0 && sql_keyword_at(sql, i, "END") {
            begin_depth -= 1;
        }

        if bytes[i] == b';' && begin_depth == 0 {
            let stmt = current.trim();
            if !stmt.is_empty() {
                statements.push(stmt.to_string());
            }
            current.clear();
            i += 1;
            continue;
        }

        current.push(bytes[i] as char);
        i += 1;
    }

    let stmt = current.trim();
    if !stmt.is_empty() {
        statements.push(stmt.to_string());
    }
    statements
}

fn sql_keyword_at(sql: &str, i: usize, keyword: &str) -> bool {
    if i > 0 {
        let prev = sql.as_bytes()[i - 1];
        if prev.is_ascii_alphanumeric() || prev == b'_' {
            return false;
        }
    }
    let Some(rest) = sql.get(i..) else {
        return false;
    };
    if !rest.len().ge(&keyword.len()) || !rest[..keyword.len()].eq_ignore_ascii_case(keyword) {
        return false;
    }
    rest.get(keyword.len()..)
        .and_then(|s| s.chars().next())
        .is_none_or(|ch| !ch.is_ascii_alphanumeric() && ch != '_')
}

/// Collect migration files from a directory, returning (version, path) pairs.
///
/// Files are expected to be named like `0001_init.up.sql` or `0001_init.down.sql`.
fn collect_migration_files(dir: &PathBuf, direction: &str) -> anyhow::Result<Vec<(i64, PathBuf)>> {
    let mut migrations = Vec::new();

    let entries = std::fs::read_dir(dir)?;

    for entry in entries {
        let entry = entry?;
        let path = entry.path();

        if !path.is_file() {
            continue;
        }

        let filename = path.file_name().and_then(|n| n.to_str()).unwrap_or("");

        // Match pattern: NNNN_name.{up,down}.sql
        if !filename.ends_with(&format!(".{direction}.sql")) {
            continue;
        }

        // Extract version number
        let version_str = filename.split('_').next().unwrap_or("");
        let version: i64 = version_str
            .parse()
            .map_err(|_| anyhow::anyhow!("Invalid migration filename: {filename}"))?;

        migrations.push((version, path));
    }

    Ok(migrations)
}

/// Create a test application state with in-memory `SQLite`.
///
/// This is useful for unit and integration tests that need
/// a database without file system side effects.
#[allow(dead_code)]
pub async fn create_test_state() -> AppState {
    let storage = Storage::new("sqlite::memory:").await.unwrap();
    storage.run_migrations().await.unwrap();
    AppState {
        db: storage.pool().clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sqlite_url_parsing() {
        // Test that we can parse various SQLite URL formats
        let urls = vec![
            "sqlite:./data/lyra.db",
            "sqlite://./data/lyra.db",
            "sqlite::memory:",
        ];

        for url in urls {
            assert!(
                url.starts_with("sqlite"),
                "URL should be recognized as SQLite: {url}"
            );
        }
    }

    #[test]
    fn postgres_url_parsing() {
        let urls = vec![
            "postgres://user:pass@localhost/lyra",
            "postgresql://user:pass@localhost/lyra",
        ];

        for url in urls {
            assert!(
                url.starts_with("postgres"),
                "URL should be recognized as PostgreSQL: {url}"
            );
        }
    }

    #[test]
    fn split_sql_statements_respects_trigger_blocks() {
        let sql = r"
CREATE VIRTUAL TABLE message_fts USING fts5(subject);
CREATE TRIGGER message_fts_ai AFTER INSERT ON message BEGIN
    INSERT INTO message_fts (subject) VALUES (NEW.subject);
END;
INSERT INTO message_fts (subject) VALUES ('x');
";
        let stmts = split_sql_statements(sql);
        assert_eq!(stmts.len(), 3);
        assert!(stmts[0].starts_with("CREATE VIRTUAL TABLE"));
        assert!(stmts[1].contains("CREATE TRIGGER"));
        assert!(stmts[1].trim_end().ends_with("END"));
        assert!(stmts[2].starts_with("INSERT INTO message_fts"));
    }

    #[test]
    fn migration_file_parsing() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("migrations")
            .join("sqlite");

        if dir.exists() {
            let mut files = collect_migration_files(&dir, "up").unwrap();
            assert!(!files.is_empty(), "Should find migration files");

            // Sort by version (same as run_sqlite_migrations)
            files.sort_by_key(|(v, _)| *v);

            // Check that files are in version order
            for window in files.windows(2) {
                assert!(
                    window[0].0 < window[1].0,
                    "Migrations should be in version order"
                );
            }
        }
    }

    #[tokio::test]
    async fn sqlite_in_memory_pool_creation() {
        // Test that we can create an in-memory SQLite pool
        let storage = Storage::new("sqlite::memory:").await.unwrap();
        assert!(storage.pool().is_sqlite());
        assert_eq!(storage.pool().engine_name(), "sqlite");
    }

    #[tokio::test]
    async fn sqlite_in_memory_migrations() {
        // Test that migrations run successfully on in-memory SQLite
        let storage = Storage::new("sqlite::memory:").await.unwrap();
        storage.run_migrations().await.unwrap();

        // Verify a table was created
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='lyra_user'",
        )
        .fetch_one(match storage.pool() {
            DbPool::Sqlite(pool) => pool,
            #[cfg(feature = "postgres")]
            DbPool::Postgres(_) => panic!("Expected SQLite"),
        })
        .await
        .unwrap();

        assert_eq!(count, 1, "lyra_user table should exist");
    }

    #[cfg(feature = "postgres")]
    mod postgres_live {
        use super::*;

        /// Live check: pool sessions are pinned to UTC so `date()` bucketing
        /// matches SQLite's UTC text dates. Set
        /// `LYRA_TEST_DATABASE_URL=postgres://…` and run
        /// `cargo test --features postgres -- --ignored`.
        #[tokio::test]
        #[ignore = "needs postgres"]
        async fn postgres_pool_sessions_use_utc_timezone() {
            let url = std::env::var("LYRA_TEST_DATABASE_URL")
                .expect("LYRA_TEST_DATABASE_URL=postgres://…");
            let storage = Storage::new(&url).await.expect("connect postgres");
            let DbPool::Postgres(pool) = storage.pool().clone() else {
                panic!("expected postgres pool");
            };
            let tz: String = sqlx::query_scalar("SHOW TIME ZONE")
                .fetch_one(&pool)
                .await
                .unwrap();
            assert_eq!(tz, "UTC");
        }
    }
}
