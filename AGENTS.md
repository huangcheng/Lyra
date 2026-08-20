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

---

## Living update checklist

Update `AGENTS.md` (and the linked spec if needed) when any of these land:

- [ ] New top-level packages / crates / apps, or a renamed layout  
- [ ] Locked stack choice added, replaced, or version-pinned in practice  
- [ ] New standard scripts (`dev`, `test`, `lint`, `migrate`, Docker entrypoints)  
- [ ] New doc under `docs/product/` or `docs/specs/` that agents must read for common tasks  
- [ ] Engineering rule learned from a bug or review (promote into standards, summarize here)

Detail lives in specs; this file stays short and accurate.

---

## Product truth

| When | Read |
|------|------|
| Scoping features, v1 boundaries, non-goals | `docs/product/2026-08-20-lyra-v1-product-spec.md` |
| Clean/robust code, deep modules, state roles, verification | `docs/specs/2026-08-20-lyra-engineering-standards.md` |
| Other design/tech decisions | `docs/specs/YYYY-MM-DD-<topic>-spec.md` as added |

Lyra is a **self-hosted mail client** (not a mail server). Prefer **JMAP**, fall back to **IMAP**. Honor v1 non-goals (no collaboration suite, no SaaS, no multi-user UX yet).

---

## Project map (keep current)

*Early repo — replace this section as code appears.*

```
Lyra/
  AGENTS.md                 ← you are here (keep in sync with the project)
  docs/
    product/                ← version / product scope
    specs/                  ← engineering & technical specs
  (apps / crates TBD)       ← update paths here when scaffolded
```

Expected shape once scaffolded (adjust when real):

| Area | Likely home | Notes |
|------|-------------|--------|
| Web UI | frontend app | React, TanStack Router, shadcn mail, en/zh i18n |
| API + sync | Rust (Axum) | Thin handlers; sync engine as a deep module |
| DB | SQLite + PostgreSQL | One schema, dual backends via ORM |

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
- Tests at module seams; format/lint before done.
- Match existing patterns; ask before replacing a locked stack choice.

---

## Docs convention

| Kind | Location |
|------|----------|
| Product / version scope | `docs/product/YYYY-MM-DD-….md` |
| Design & technical specs | `docs/specs/YYYY-MM-DD-<topic>-spec.md` |

Decisions made in chat belong in the right doc before the thread ends — not only in conversation history.
