# Lyra Backend

Rust + Axum backend for the Lyra mail client.

**OpenGPG:** key parse/store uses the [`pgp`](https://crates.io/crates/pgp) (rPGP) crate — pure Rust, MIT/Apache. Keys API: `/api/v1/opengpg/keys`; unlock/lock: `/api/v1/opengpg/unlock` + `/lock` (per-session passphrase cache). See `docs/specs/2026-08-23-lyra-opengpg-spec.md`.

## Quick start

```bash
cd backend

# Required secret (no default; boot fails without it)
export LYRA_MASTER_KEY=$(openssl rand -base64 32)

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
| `LYRA_MASTER_KEY` | *(required)* | Master key (32+ bytes) for the per-user DEK hierarchy that encrypts stored account credentials and TOTP secrets. Boot fails without it. Generate: `openssl rand -base64 32` |
| `LISTEN_ADDR` | `0.0.0.0:3000` | Address and port to listen on |
| `DATABASE_URL` | `sqlite:./data/lyra.db` | Database connection string |
| `DATA_DIR` | `./data` | Directory for message blobs and attachments |
| `REDIS_URL` | unset (in-memory kv) | Redis for sessions/jobs; omit for process-local memory |
| `FRONTEND_DIR` | `frontend/dist` | Built SPA; missing dir → API-only |
| `MIGRATIONS_DIR` | auto | Override path to `migrations/{sqlite,postgres}` |
| `RUST_LOG` | `info` | Log level filter (tracing-subscriber) |
| `LYRA_MS_OAUTH_CLIENT_ID` | unset | Microsoft Entra app client ID (enables Outlook OAuth) |
| `LYRA_MS_OAUTH_CLIENT_SECRET` | unset | App secret (confidential clients; omit for public+PKCE-only) |
| `LYRA_MS_OAUTH_REDIRECT_URI` | unset | Must match Entra redirect, e.g. `http://localhost:3000/api/v1/oauth/microsoft/callback` |
| `LYRA_MS_OAUTH_TENANT` | `common` | Entra tenant (`common`, `organizations`, or tenant GUID) |

When Microsoft OAuth is configured, Settings → Accounts shows **Sign in with Microsoft**. Tokens are encrypted under the user DEK (`auth_type = oauth2`); IMAP/SMTP use SASL XOAUTH2 with automatic refresh.

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
  main.rs       ← Axum app, health + version + SPA, entry point
  config.rs     ← Environment-based configuration
  auth.rs       ← Username/password + optional TOTP; bearer sessions in kv
  storage.rs    ← Storage seam (SQLite + PostgreSQL, transactions)
  sync/         ← HTTP, IMAP/JMAP loops, persist batches, SMTP send
  imap.rs / jmap.rs / smtp.rs
  jobs.rs / scheduler.rs / kernel/
```

This is a **binary crate**. Run tests with `cargo test --bin lyra_backend` (not `--lib`).

Each of `auth`, `accounts`, `sync`, and `pim` exposes a `routes()` function. The main router merges them under `/api/v1` (`/health` and `/version` stay unversioned).

## Lint & format

```bash
cargo fmt                     # Format
cargo clippy -- -D warnings   # Lint (warnings as errors)
cargo test --bin lyra_backend   # Unit tests (binary crate)
```

Or from the repo root: `make backend-fmt` / `make backend-lint`.

## Specs

- [Data model (dual-DB)](../docs/specs/2026-08-20-lyra-data-model-spec.md)
- [Sync engine & protocols](../docs/specs/2026-08-20-lyra-sync-and-protocols-spec.md)
- [Engineering standards](../docs/specs/2026-08-20-lyra-engineering-standards.md)

## Auth API demo

Quick curl script demonstrating the auth flow end-to-end:

```bash
BASE=http://localhost:3000

# 1. Bootstrap first user (only works when no user exists)
echo "=== Bootstrap ==="
curl -s -X POST $BASE/api/v1/auth/bootstrap \
  -H 'Content-Type: application/json' \
  -d '{"username":"admin","password":"Str0ngP@ss"}' | jq .
# Returns: {"token":"...","user":{...},"requires_totp":false}

# 2. Login with credentials
echo "=== Login ==="
RESP=$(curl -s -X POST $BASE/api/v1/auth/login \
  -H 'Content-Type: application/json' \
  -d '{"username":"admin","password":"Str0ngP@ss"}')
echo $RESP | jq .
TOKEN=$(echo $RESP | jq -r '.token')

# 3. Call protected route with token
echo "=== Protected route (with token) ==="
curl -s $BASE/api/v1/storage/status \
  -H "Authorization: Bearer $TOKEN" | jq .
# Returns: {"engine":"sqlite","ready":true}

# 4. Call protected route without token → 401
echo "=== Protected route (no token) ==="
curl -s -w '\nHTTP %{http_code}' $BASE/api/v1/storage/status
# Returns 401 Unauthorized

# 5. Check auth status
echo "\n=== Auth status ==="
curl -s $BASE/api/v1/auth/status | jq .
# Returns: {"has_user":true,"totp_enabled":false}

# 6. Get current user
echo "=== Current user ==="
curl -s $BASE/api/v1/auth/me \
  -H "Authorization: Bearer $TOKEN" | jq .

# 7. Logout
echo "=== Logout ==="
curl -s -X POST $BASE/api/v1/auth/logout \
  -H "Authorization: Bearer $TOKEN" -w '\nHTTP %{http_code}'
```

## Accounts API demo

Manage mail accounts with authenticated Bearer token:

```bash
BASE=http://localhost:3000

# Login first (get token)
RESP=$(curl -s -X POST $BASE/api/v1/auth/login \
  -H 'Content-Type: application/json' \
  -d '{"username":"admin","password":"Str0ngP@ss"}')
TOKEN=$(echo $RESP | jq -r '.token')

# ─── Create first account (personal) ───────────────────────────────
echo "=== Create Account 1 ==="
curl -s -X POST $BASE/api/v1/accounts \
  -H 'Content-Type: application/json' \
  -H "Authorization: Bearer $TOKEN" \
  -d '{
    "displayName": "Personal Mail",
    "emailAddress": "user@example.com",
    "password": "account-password-here",
    "protocol": "imap",
    "imapHost": "imap.example.com",
    "imapPort": 993,
    "imapSecurity": "tls",
    "smtpHost": "smtp.example.com",
    "smtpPort": 587,
    "smtpSecurity": "starttls"
  }' | jq .

# ─── Create second account (work) ──────────────────────────────────
echo "=== Create Account 2 ==="
curl -s -X POST $BASE/api/v1/accounts \
  -H 'Content-Type: application/json' \
  -H "Authorization: Bearer $TOKEN" \
  -d '{
    "displayName": "Work Mail",
    "emailAddress": "user@work.example.com",
    "password": "work-password-here",
    "protocol": "imap",
    "imapHost": "mail.work.example.com",
    "imapPort": 993,
    "imapSecurity": "tls",
    "smtpHost": "smtp.work.example.com",
    "smtpPort": 465,
    "smtpSecurity": "tls"
  }' | jq .

# ─── List all accounts ─────────────────────────────────────────────
echo "=== List Accounts ==="
curl -s $BASE/api/v1/accounts \
  -H "Authorization: Bearer $TOKEN" | jq .

# ─── Get single account ────────────────────────────────────────────
ACCOUNT_ID="<paste-id-from-list>"
echo "=== Get Account ==="
curl -s $BASE/api/v1/accounts/$ACCOUNT_ID \
  -H "Authorization: Bearer $TOKEN" | jq .

# ─── Update account ────────────────────────────────────────────────
echo "=== Update Account ==="
curl -s -X PUT $BASE/api/v1/accounts/$ACCOUNT_ID \
  -H 'Content-Type: application/json' \
  -H "Authorization: Bearer $TOKEN" \
  -d '{"displayName": "My Personal Mail"}' | jq .

# ─── Probe server config ───────────────────────────────────────────
echo "=== Probe Server Config ==="
curl -s -X POST $BASE/api/v1/accounts/probe \
  -H 'Content-Type: application/json' \
  -H "Authorization: Bearer $TOKEN" \
  -d '{"emailAddress": "user@gmail.com"}' | jq .

# ─── Delete account ────────────────────────────────────────────────
echo "=== Delete Account ==="
curl -s -X DELETE $BASE/api/v1/accounts/$ACCOUNT_ID \
  -H "Authorization: Bearer $TOKEN" -w '\nHTTP %{http_code}'
```
