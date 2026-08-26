# Lyra — Agent Guidance

These instructions apply to the entire repository.

**`AGENTS.md` is a living document.** When the repo’s layout, stack, commands, or conventions change, update this file in the same change (or immediately after). Stale agent guidance is a defect.

## Open-source hygiene (public repo)

Lyra is intended to be **open source**. Treat the tree as public forever.

- **Never commit** secrets, tokens, API keys, credentials, private emails, or `.env` values.
- **Never commit** private project-management data: issue tracker URLs, workspace/org names, private issue IDs, board links, or agent/run URLs from internal tools.
- Keep task tracking **outside** the public tree (or only in local-ignored paths). Commits and docs describe *what* changed, not private ticket links.
- Prefer example.com / placeholders in docs and tests; no real mail accounts or hostnames that identify private infra.
- Before committing, scan the diff for accidental PII, tokens, and tracker references.
- **Secret-leak enforcement** is automated via gitleaks (see below). Install pre-commit hooks before contributing.

### Secret-leak enforcement (gitleaks)

| Tool | Purpose |
|------|---------|
| `.gitleaks.toml` | Config: default rules + custom `linear.app` URL deny + AWS/key patterns |
| `.pre-commit-config.yaml` | Pre-commit hook (gitleaks runs on every commit) |
| `scripts/secretscan.sh` | Standalone full-history scanner |
| `make secretscan` | Entry point for full-history scan |

**Quick start (contributors):**
```bash
brew install gitleaks          # or: https://github.com/gitleaks/gitleaks#installing
pip install pre-commit         # or: brew install pre-commit
make pre-commit-install        # activate gitleaks (pre-commit + commit-msg hooks)
pre-commit run --all-files     # verify hooks without committing
make secretscan                # run a one-off full-history scan
```

Pre-commit runs **gitleaks only**; `make check` covers format, lint, and full-history secret scan.

Every commit is automatically scanned. CI should also run `make secretscan` on the full history.

---

## Living update checklist

Update `AGENTS.md` (and the linked spec if needed) when any of these land:

- [x] New top-level packages / crates / apps, or a renamed layout  
- [x] Locked stack choice added, replaced, or version-pinned in practice  
- [x] Standard scripts: `make fmt`, `make lint`, `make check`, `make secretscan`  
- [x] Secret-scan tooling: `.gitleaks.toml`, `.pre-commit-config.yaml`, `scripts/secretscan.sh`, `Makefile`  
- [x] New doc under `docs/product/` or `docs/specs/` that agents must read for common tasks  
- [x] Engineering rule learned from a bug or review (promote into standards, summarize here)

Detail lives in specs; this file stays short and accurate.

---

## Product truth

| When | Read |
|------|------|
| Scoping features, v1 boundaries, non-goals | `docs/product/2026-08-20-lyra-v1-product-spec.md` |
| Far-horizon multi-client order (API → desktop → mobile) | `docs/product/2026-08-20-lyra-multi-client-roadmap.md` |
| Post-v1 AI assist (BYOK; draft → categorize → spam → calendar) | `docs/product/2026-08-21-lyra-ai-assist-roadmap.md` |
| Clean/robust code, deep modules, state roles, verification | `docs/specs/2026-08-20-lyra-engineering-standards.md` |
| HTTP API surface (`/api/v1`, errors, web client boundary, v2 policy) | `docs/specs/2026-08-26-lyra-http-api-surface.md` |
| OpenAPI contract for `/api/v1` | `docs/openapi/api-v1.yaml` |
| Data model, dual-DB schema, migrations | `docs/specs/2026-08-20-lyra-data-model-spec.md` |
| Sync engine, protocols, auto-config | `docs/specs/2026-08-20-lyra-sync-and-protocols-spec.md` |
| Plugin kernel, workers, jobs/snooze, Redis kv | `docs/specs/2026-08-22-lyra-plugin-kernel-spec.md` |
| OpenGPG keys, decrypt/verify, sign/encrypt (phased; OpenPGP wire format, GnuPG interop) | `docs/specs/2026-08-23-lyra-opengpg-spec.md` |
| Remote-image proxy / anti-tracking (phased) | `docs/specs/2026-08-23-lyra-remote-image-proxy-spec.md` |
| Other design/tech decisions | `docs/specs/YYYY-MM-DD-<topic>-spec.md` as added |
| Product UI (redesign v2: stamp brand, folder-tree mail, standalone dashboard/settings) | `docs/superpowers/specs/2026-08-24-lyra-redesign-v2-design.md` |

Lyra is a **self-hosted mail client** (not a mail server). Prefer **JMAP**, fall back to **IMAP**. Honor v1 non-goals (no collaboration suite, no SaaS, no multi-user UX yet). Ship a **client-agnostic `/api/v1`**; native apps are far-horizon only.

---

## Project map (keep current)

```
Lyra/
  AGENTS.md
  Makefile                      ← fmt / lint / check / secretscan
  PRODUCT.md / DESIGN.md / DESIGN.json  ← impeccable design context (brand + visual system); read before UI work
  docs/
    product/                    ← product spec + far-horizon roadmaps (multi-client, AI assist)
    specs/                      ← data model, sync, engineering standards
  frontend/                     ← Vite + React + TanStack Router + shadcn mail
    src/
      components/               ← shadcn/ui + mail example (unified inbox)
      stores/                   ← Zustand (mail data, UI state)
      machines/                 ← XState (auth flow)
      lib/                      ← `/api/v1` client, session restore, mappers
      rxjs/                     ← RxJS (SSE sync event stream)
      i18n/                     ← en + zh translations
  backend/                      ← Rust + Axum
    src/
      main.rs                   ← health + version routes, SPA, entry point
      config.rs                 ← env-based configuration (`LYRA_MASTER_KEY` required)
      auth.rs                   ← username/password + optional TOTP, bearer sessions
      storage.rs                ← storage seam (SQLite + PostgreSQL, migrations)
      sync/                     ← sync HTTP, IMAP/JMAP loops, persist transactions
      imap.rs / jmap.rs / smtp.rs
      oauth/ ← Microsoft mail OAuth (PKCE) + XOAUTH2 token resolve
      jobs.rs / scheduler.rs / kernel/
    migrations/
      sqlite/                   ← SQLite migration SQL files
      postgres/                 ← PostgreSQL migration SQL files
    README.md                   ← how to run + migration docs
  scripts/
    secretscan.sh               ← gitleaks scanner
```

| Area | Path | Notes |
|------|------|--------|
| Web UI | `frontend/` | React, TanStack Router, Tailwind + shadcn mail, en/zh i18n; typed `api()` client |
| API + sync | `backend/` | Rust + Axum; `/api/v1`; health + version unversioned |
| Tests | `backend/` | Binary crate: `cargo test --bin lyra_backend` (not `--lib`) |
| DB | `backend/migrations/` | sqlx; SQLite + PostgreSQL; auto-migrate on startup |

### Lint & format

| Command | What it does |
|---------|----------------|
| `make fmt` | Prettier (frontend) + `cargo fmt` (backend) |
| `make lint` | oxlint + tsc (frontend) + clippy `-D warnings` (backend) |
| `make check` | format check + lint + secret scan |
| `cd frontend && npm run check` | frontend only |
| `cd backend && cargo clippy --all-targets --all-features -- -D warnings` | backend only |
| `cd backend && cargo test --bin lyra_backend` | backend unit tests |

---

## Stack (locked for v1)

| Layer | Choice |
|-------|--------|
| Frontend | React, TanStack Router, shadcn mail as the product UI |
| i18n | English + Chinese (zh) |
| Client state | Zustand (data) · XState (flows) · RxJS (async / recovery) |
| Backend | Rust + Axum |
| DB | SQLite **and** PostgreSQL |
| Auth | Username/password + optional TOTP |
| Deploy | Docker Compose (recommended) + install script |

---

## Always-on engineering (summary)

Full rules: `docs/specs/2026-08-20-lyra-engineering-standards.md`.

- **Deep modules** at real seams; hide protocols, SQL, and crypto.
- **Protocol adapters** (IMAP, JMAP, SMTP, POP3) MUST follow their RFC/JMAP specs — wire vs display, capability negotiation, opaque cursors; checklist in sync spec §13.
- Sync **idempotent** and resumable; typed errors; no secret logging.
- Handlers thin; schema dual-DB; single-user now, multi-user-ready data shape.
- **HTTP API is the UI surface** (`/api/v1`); web is a peer client — no web-only backend shortcuts.
- Tests at module seams; format/lint before done (`make fmt` / `make lint` / `make check`).
- Match existing patterns; ask before replacing a locked stack choice.
- **Secrets never in tree** — gitleaks enforced via pre-commit + `make secretscan`.

---

## Execution

- Prefer **local** development and agent runs against a local checkout.
- GitHub is the public source of truth; mirrors elsewhere are optional.
- Do not document or commit private personal tooling, workflows, or tracker identifiers.

---

## Docs convention

| Kind | Location |
|------|----------|
| Product / version scope | `docs/product/YYYY-MM-DD-….md` |
| Design & technical specs | `docs/specs/YYYY-MM-DD-<topic>-spec.md` |

Decisions made in chat belong in the right doc before the thread ends — not only in conversation history.
