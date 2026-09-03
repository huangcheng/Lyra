# Lyra

Self-hosted mail **client** (not a mail server). Prefer JMAP, fall back to IMAP;
send via SMTP. One user today, multi-user-ready data shape. Web UI is English +
Chinese.

Tagline: *Mail you host yourself.*

## Quick start (Docker Compose)

```bash
cp .env.example .env
# Required:
printf 'LYRA_MASTER_KEY=%s\n' "$(openssl rand -base64 32)" >> .env
printf 'LYRA_PUBLIC_URL=%s\n' 'http://localhost:3000' >> .env

docker compose up --build -d
curl -s http://127.0.0.1:3000/health
# open http://localhost:3000 — bootstrap the first user, then add mail accounts
```

Full deploy notes (Postgres, HTTPS, install script): [`deploy/README.md`](deploy/README.md).

## Local development

| Area | Path | Notes |
|------|------|--------|
| API + sync | `backend/` | Rust + Axum; `/api/v1`; see [`backend/README.md`](backend/README.md) |
| Web UI | `frontend/` | Vite + React; see [`frontend/README.md`](frontend/README.md) |
| Dual-DB migrations | `backend/migrations/{sqlite,postgres}/` | Auto-run on boot |

```bash
# Backend (requires LYRA_MASTER_KEY + LYRA_PUBLIC_URL)
cd backend && cargo run

# Frontend (proxies /api to :3000)
cd frontend && npm install && npm run dev
```

Common checks from the repo root:

| Command | What it does |
|---------|----------------|
| `make fmt` | Prettier + `cargo fmt` |
| `make lint` | oxlint + tsc + clippy |
| `make test` | vitest + `cargo test --bin lyra_backend` |
| `make check` | format check + lint + tests + secret scan |
| `make secretscan` | gitleaks full-history scan |

Install commit hooks: `make pre-commit-install` (gitleaks on every commit).

## What you get

- Multi-account unified inbox (IMAP / JMAP / SMTP)
- Conversations, search, move / **same-account copy**, trash / archive / spam
- Sync progress + Settings → Accounts **error log** (scrubbed IMAP/JMAP detail)
- OpenGPG keys, remote-image privacy controls, Microsoft / Yandex mail OAuth
- SQLite (default) or PostgreSQL; Redis optional for sessions/jobs

## Docs

| When | Read |
|------|------|
| Product scope / v1 finish line | [`docs/product/`](docs/product/) |
| HTTP API + OpenAPI | [`docs/specs/2026-08-26-lyra-http-api-surface.md`](docs/specs/2026-08-26-lyra-http-api-surface.md), [`docs/openapi/api-v1.yaml`](docs/openapi/api-v1.yaml) |
| Data model, sync, engineering | [`docs/specs/`](docs/specs/) |
| Agent / contributor map | [`AGENTS.md`](AGENTS.md) |

## Security notes

- Never commit `.env`, tokens, or real mail credentials (gitleaks enforced).
- `LYRA_MASTER_KEY` encrypts stored account credentials and TOTP secrets — losing it means re-adding accounts.
- Do not expose plain HTTP on the public internet; terminate TLS at a reverse proxy.
