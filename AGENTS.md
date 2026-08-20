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
pre-commit install             # activate the gitleaks hook
make secretscan                # run a one-off full-history scan
```

Every commit is automatically scanned. CI should also run `make secretscan` on the full history.

---

## Living update checklist

Update `AGENTS.md` (and the linked spec if needed) when any of these land:

- [ ] New top-level packages / crates / apps, or a renamed layout  
- [ ] Locked stack choice added, replaced, or version-pinned in practice  
- [x] Standard scripts: `make fmt`, `make lint`, `make check`, `make secretscan`  
- [x] Secret-scan tooling: `.gitleaks.toml`, `.pre-commit-config.yaml`, `scripts/secretscan.sh`, `Makefile`  
- [x] New doc under `docs/product/` or `docs/specs/` that agents must read for common tasks  
- [ ] Engineering rule learned from a bug or review (promote into standards, summarize here)

Detail lives in specs; this file stays short and accurate.

---

## Product truth

| When | Read |
|------|------|
| Scoping features, v1 boundaries, non-goals | `docs/product/2026-08-20-lyra-v1-product-spec.md` |
| Clean/robust code, deep modules, state roles, verification | `docs/specs/2026-08-20-lyra-engineering-standards.md` |
| Data model, dual-DB schema, migrations | `docs/specs/2026-08-20-lyra-data-model-spec.md` |
| Sync engine, protocols, auto-config | `docs/specs/2026-08-20-lyra-sync-and-protocols-spec.md` |
| Other design/tech decisions | `docs/specs/YYYY-MM-DD-<topic>-spec.md` as added |

Lyra is a **self-hosted mail client** (not a mail server). Prefer **JMAP**, fall back to **IMAP**. Honor v1 non-goals (no collaboration suite, no SaaS, no multi-user UX yet).

---

## Project map (keep current)

```
Lyra/
  AGENTS.md
  Makefile                      ← fmt / lint / check / secretscan
  docs/
    product/                    ← product spec
    specs/                      ← data model, sync, engineering standards
  frontend/                     ← Vite + React + TanStack Router + shadcn mail
    src/
      components/               ← three-pane mail chrome (sidebar, list, view)
      stores/                   ← Zustand (mail data, UI state)
      machines/                 ← XState (auth, account-setup flows)
      rxjs/                     ← RxJS (sync event streams)
      i18n/                     ← en + zh translations
  backend/                      ← Rust + Axum
    src/
      main.rs                   ← health + version routes, entry point
      config.rs                 ← env-based configuration
      auth.rs                   ← authentication stub
      storage.rs                ← repository stub (SQLite + PostgreSQL)
      sync.rs                   ← sync engine stub (JMAP/IMAP/SMTP)
    README.md                   ← how to run
  scripts/
    secretscan.sh               ← gitleaks scanner
```

| Area | Path | Notes |
|------|------|--------|
| Web UI | `frontend/` | React, TanStack Router, shadcn mail, en/zh i18n |
| API + sync | `backend/` | Rust + Axum; health + version routes, module stubs |
| DB | TBD | SQLite + PostgreSQL |

### Lint & format

| Command | What it does |
|---------|----------------|
| `make fmt` | Prettier (frontend) + `cargo fmt` (backend) |
| `make lint` | oxlint + tsc (frontend) + clippy `-D warnings` (backend) |
| `make check` | format check + lint + secret scan |
| `cd frontend && npm run check` | frontend only |
| `cd backend && cargo clippy -- -D warnings` | backend only |

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
- Sync **idempotent** and resumable; typed errors; no secret logging.
- Handlers thin; schema dual-DB; single-user now, multi-user-ready data shape.
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
