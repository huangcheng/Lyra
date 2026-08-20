# Lyra — Engineering Standards

**Date:** 2026-08-20  
**Status:** Active  
**Companion:** Always-on summary lives in repo-root `AGENTS.md`. Update both when conventions change.

---

## Deep modules

Design **deep modules**: a lot of behaviour behind a small **interface**, at a clear **seam**.

| Term | Meaning |
|------|---------|
| Module | Anything with an interface and an implementation (function, package, crate slice) |
| Interface | Everything a caller must know: types, invariants, errors, ordering, performance |
| Seam | Where behaviour can change without editing callers (Feathers) |
| Adapter | Concrete thing that satisfies an interface at a seam |

Rules:

- Hide protocol details (JMAP/IMAP/SMTP), SQL, and crypto behind small interfaces.
- Introduce a seam only when something **actually varies** (e.g. SQLite vs Postgres). One adapter = hypothetical; two adapters = real seam.
- Prefer one owner per concern — fix once, fixed everywhere.
- The interface is the test surface: callers and tests cross the same seam.

## Size and structure

- Keep source files focused. Warn past ~500 lines; treat sprawling god-files as a defect and split along natural seams.
- Colocate tests with the module seam they exercise.
- Name modules by responsibility (`sync`, `imap`, `jmap`, `auth`), not by layer soup (`utils`, `helpers`, `common` dumping grounds).

## Robustness

- Sync must be **idempotent** and safe to resume after crash or network loss.
- Use typed errors with recovery paths at protocol and sync seams.
- Surface actionable UI states via XState / RxJS — not ad-hoc toast spam from scattered `catch` blocks.
- Encrypt mail-account credentials at the storage boundary; never log secrets or plaintext passwords.
- Validate at boundaries (HTTP, protocol parsers, user input); trust less inward.
- Empty, loading, offline, and auth-expired states are part of the feature.

## Frontend

- Extend the [shadcn mail](https://v3.shadcn.com/examples/mail) UI; do not redesign mail chrome from scratch.
- All user-visible copy through i18n (**en** + **zh**).
- **Zustand** — UI/domain data views read and write.
- **XState** — multi-step flows (onboarding, auth, account setup, sync lifecycle).
- **RxJS** — long-lived async pipelines, retries, backpressure, error recovery.
- Do not put the same logic in two of these three.
- Add Zustand slices only when multiple views share real state.

## Backend

- Axum handlers stay thin; business logic lives in modules behind seams.
- Migrations and schema must work on both **SQLite** and **PostgreSQL**.
- Shape data for single-user now, with ownership keys (or equivalent) so multi-user can land later — without building multi-user UX in v1.

## Verification

- Test through the module interface (sync, protocol adapters, auth), not only UI snapshots.
- Prefer a failing test that proves the bug, then the fix.
- Before considering work done: `make fmt` / `make lint` (or `make check`). Frontend uses **oxlint** + **Prettier**; backend uses **rustfmt** + **clippy** (`-D warnings`).
- **Run `make secretscan`** (gitleaks) before merging or releasing. Pre-commit hooks enforce this locally; CI should run the full-history scan.

## Public-repo hygiene

Lyra is open source. Do not put secrets, private tracker URLs/IDs, workspace names, or personal contact data in commits, docs, logs, or fixtures. Task systems stay outside the published tree; commit messages describe the change, not private ticket links.

## When this doc changes

Update this file **and** the summary bullets / pointers in `AGENTS.md` whenever:

- A stack choice is locked or replaced  
- A new architectural seam or package layout ships  
- Testing, lint, or CI commands become the project standard  
- A robustness rule is added from a real incident  
