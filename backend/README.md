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

## Module layout

```
src/
  main.rs       ← Axum app, health + version routes, entry point
  config.rs     ← Environment-based configuration
  auth.rs       ← Authentication stub (username/password + TOTP)
  storage.rs    ← Storage/repository stub (SQLite + PostgreSQL)
  sync.rs       ← Sync engine stub (JMAP/IMAP/SMTP)
```

Each module exposes a `routes()` function that returns an Axum `Router`. The main router merges them all.

## Lint & format

```bash
cargo fmt                  # Format
cargo clippy -- -D warnings  # Lint (warnings as errors)
```

Or from the repo root: `make backend-fmt` / `make backend-lint`.

## Specs

- [Data model (dual-DB)](../docs/specs/2026-08-20-lyra-data-model-spec.md)
- [Sync engine & protocols](../docs/specs/2026-08-20-lyra-sync-and-protocols-spec.md)
- [Engineering standards](../docs/specs/2026-08-20-lyra-engineering-standards.md)
