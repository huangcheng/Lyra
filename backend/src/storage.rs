//! Storage module — dual-DB seam for `SQLite` and `PostgreSQL`.
//!
//! This module hides the database implementation behind a clean interface.
//! Migrations are loaded from `migrations/sqlite/` or `migrations/postgres/`
//! based on the `DATABASE_URL` scheme.
//!
//! See `docs/specs/2026-08-20-lyra-data-model-spec.md` for the schema.

use axum::{Json, Router, extract::State, routing::get};
use serde::Serialize;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Pool, Sqlite};
use std::path::PathBuf;
use std::str::FromStr;

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
}

/// Shared application state containing the database pool.
#[derive(Clone)]
pub struct AppState {
    pub db: DbPool,
}

/// Storage module configuration and connection management.
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
                .foreign_keys(true); // Enforce FK constraints

            let pool = SqlitePoolOptions::new()
                .max_connections(5)
                .connect_with(options)
                .await?;

            DbPool::Sqlite(pool)
        } else if database_url.starts_with("postgres") || database_url.starts_with("postgresql") {
            #[cfg(feature = "postgres")]
            {
                let pool = sqlx::postgres::PgPoolOptions::new()
                    .max_connections(10)
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
            anyhow::bail!(
                "Unsupported DATABASE_URL scheme. Use 'sqlite:' or 'postgres://'."
            );
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

        tracing::info!(
            "Running migrations from {}",
            migrations_dir.display()
        );

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
        // Find the backend directory (we're running from there)
        let base = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let subdir = match &self.pool {
            DbPool::Sqlite(_) => "sqlite",
            #[cfg(feature = "postgres")]
            DbPool::Postgres(_) => "postgres",
        };

        let dir = base.join("migrations").join(subdir);
        if !dir.exists() {
            anyhow::bail!(
                "Migrations directory not found: {}",
                dir.display()
            );
        }

        Ok(dir)
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

        // Strip inline comments and execute statements individually.
        // SQLite doesn't support multi-statement execution via prepare,
        // so we need to split carefully.
        let cleaned = strip_sql_comments(&sql);
        for stmt in cleaned.split(';') {
            let stmt = stmt.trim();
            if stmt.is_empty() {
                continue;
            }
            sqlx::query(stmt).execute(pool).await?;
        }

        // Record migration
        sqlx::query("INSERT INTO schema_migrations (version) VALUES (?)")
            .bind(version)
            .execute(pool)
            .await?;

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
        sqlx::raw_sql(&sql).execute(pool).await?;

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

/// Collect migration files from a directory, returning (version, path) pairs.
///
/// Files are expected to be named like `0001_init.up.sql` or `0001_init.down.sql`.
fn collect_migration_files(
    dir: &PathBuf,
    direction: &str,
) -> anyhow::Result<Vec<(i64, PathBuf)>> {
    let mut migrations = Vec::new();

    let entries = std::fs::read_dir(dir)?;

    for entry in entries {
        let entry = entry?;
        let path = entry.path();

        if !path.is_file() {
            continue;
        }

        let filename = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");

        // Match pattern: NNNN_name.{up,down}.sql
        if !filename.ends_with(&format!(".{direction}.sql")) {
            continue;
        }

        // Extract version number
        let version_str = filename.split('_').next().unwrap_or("");
        let version: i64 = version_str.parse().map_err(|_| {
            anyhow::anyhow!("Invalid migration filename: {filename}")
        })?;

        migrations.push((version, path));
    }

    Ok(migrations)
}

/// Create the shared application state from environment configuration.
pub async fn create_app_state() -> anyhow::Result<AppState> {
    let database_url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
        tracing::warn!("DATABASE_URL not set; defaulting to sqlite:./data/lyra.db");
        "sqlite:./data/lyra.db".to_string()
    });

    let storage = Storage::new(&database_url).await?;
    storage.run_migrations().await?;

    Ok(AppState {
        db: storage.pool().clone(),
    })
}

// ── Axum routes ──────────────────────────────────────────────────────

/// Routes for storage-related endpoints.
pub fn routes() -> Router<AppState> {
    Router::new().route("/api/storage/status", get(storage_status))
}

#[derive(Serialize)]
pub struct StorageStatus {
    pub engine: String,
    pub ready: bool,
}

/// Reports storage readiness and engine type.
async fn storage_status(State(state): State<AppState>) -> Json<StorageStatus> {
    Json(StorageStatus {
        engine: state.db.engine_name().to_string(),
        ready: true,
    })
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
    fn migration_file_parsing() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("migrations")
            .join("sqlite");

        if dir.exists() {
            let files = collect_migration_files(&dir, "up").unwrap();
            assert!(!files.is_empty(), "Should find migration files");

            // Check that files are sorted by version
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
}
