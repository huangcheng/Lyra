# Lyra Backend

Rust + Axum backend for the Lyra mail client.

## Quick start

```bash
cd backend

# Build and run (defaults to 0.0.0.0:3000 with SQLite)
cargo run

# Check health
curl http://localhost:3000/health
# → {"status":"ok"}

# Check version
curl http://localhost:3000/version
# → {"version":"0.1.0","name":"lyra_backend"}
```

## Configuration

All config via environment variables:

| Variable | Default | Description |
|----------|---------|-------------|
| `LISTEN_ADDR` | `0.0.0.0:3000` | Address and port to listen on |
| `DATABASE_URL` | `sqlite:./data/lyra.db` | Database connection string |
| `DATA_DIR` | `./data` | Directory for message blobs and attachments |
| `RUST_LOG` | `info` | Log level filter (tracing-subscriber) |

### Database URLs

**SQLite (default):**
```bash
# File-based SQLite
DATABASE_URL=sqlite:./data/lyra.db

# In-memory SQLite (for testing)
DATABASE_URL=sqlite::memory:
```

**PostgreSQL:**
```bash
DATABASE_URL=postgres://user:password@localhost:5432/lyra
```

## Database & Migrations

### Migration structure

Migrations live in `backend/migrations/` with separate directories per database backend:

```
backend/migrations/
├── sqlite/
│   ├── 0001_init.up.sql
│   └── 0001_init.down.sql
└── postgres/
    ├── 0001_init.up.sql
    └── 0001_init.down.sql
```

The migration runner automatically selects the correct directory based on `DATABASE_URL`.

### Running migrations

Migrations run automatically when the server starts. To run them manually:

```bash
# SQLite (creates data/lyra.db if it doesn't exist)
DATABASE_URL=sqlite:./data/lyra.db cargo run

# PostgreSQL (requires postgres feature)
DATABASE_URL=postgres://localhost/lyra cargo run --features postgres
```

### Verifying SQLite migrations

```bash
# Run with SQLite and check logs
DATABASE_URL=sqlite:./data/lyra.db RUST_LOG=debug cargo run

# Query the database directly
sqlite3 data/lyra.db ".tables"
# Should show: attachment, calendar_event, contact, folder, lyra_user,
#              mail_account, message, schema_migrations, sync_cursor, thread

sqlite3 data/lyra.db "SELECT * FROM schema_migrations;"
# Should show applied migration version(s)
```

### Verifying PostgreSQL migrations (Docker)

```bash
# Start PostgreSQL in Docker
docker run -d --name lyra-pg \
  -e POSTGRES_USER=lyra \
  -e POSTGRES_PASSWORD=lyra \
  -e POSTGRES_DB=lyra \
  -p 5432:5432 \
  postgres:16-alpine

# Run with PostgreSQL (requires postgres feature)
DATABASE_URL=postgres://lyra:lyra@localhost:5432/lyra \
  cargo run --features postgres

# Check the database
docker exec -it lyra-pg psql -U lyra -d lyra -c "\dt"
# Should show all tables

# Cleanup
docker rm -f lyra-pg
```

## Module layout

```
src/
  main.rs       ← Axum app, health + version routes, entry point
  config.rs     ← Environment-based configuration
  auth.rs       ← Authentication stub (username/password + TOTP)
  storage.rs    ← Storage seam (SQLite + PostgreSQL, migrations)
  sync.rs       ← Sync engine stub (JMAP/IMAP/SMTP)
```

Each module exposes a `routes()` function that returns an Axum `Router<AppState>`. The main router merges them all.

## Lint & format

```bash
cargo fmt                     # Format
cargo clippy -- -D warnings   # Lint (warnings as errors)
cargo test                    # Run tests
```

Or from the repo root: `make backend-fmt` / `make backend-lint`.

## Specs

- [Data model (dual-DB)](../docs/specs/2026-08-20-lyra-data-model-spec.md)
- [Sync engine & protocols](../docs/specs/2026-08-20-lyra-sync-and-protocols-spec.md)
- [Engineering standards](../docs/specs/2026-08-20-lyra-engineering-standards.md)
