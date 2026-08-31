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
- Data access goes through SeaORM 2.0 entities (`backend/src/entities/`) — the CI postgres job enforces the dual-DB invariant on every PR; dialect-specific raw SQL is allowed only for FTS and engine-unique statements, behind an explicit backend tag.
- Shape data for single-user now, with ownership keys (or equivalent) so multi-user can land later — without building multi-user UX in v1.

### Dual-DB dialect rules (learned the hard way)

SQLite's loose typing forgives SQL that PostgreSQL refuses to even prepare. Every statement touching these shapes must be written dialect-safe from the start, and any new sync/storage seam function gets a `sync::store::postgres_live`-style roundtrip (`#[ignore = "needs postgres"]`, run by the CI postgres job):

- **JSON columns** (`from_address`, `to_addresses`, `cc_addresses`, `flags`, … — JSONB on PG, TEXT on SQLite) must be wrapped in `CAST(col AS text)` before any text operator (`LIKE`, `= ''`). Raw `col LIKE` is a prepare-time `operator does not exist: jsonb ~~ text`.
- **`ON CONFLICT … DO UPDATE` bodies**: every current-row column reference must be table-qualified (`"message"."subject"`); a bare reference is ambiguous against `excluded` on PostgreSQL. SQLite accepts the qualified form for the current row.
- **UUID id columns** bind and decode dialect-aware: writes via `id_value()` (TEXT on SQLite, native Uuid on PG); reads via `CAST(id AS text)` projections or the `tx_fetch_id`-style per-pool decode — never a raw `try_get::<String>` on an id column.
- Timestamps on the `jobs` table stay TEXT on both dialects; do not "normalize" them to native timestamps.

## Protocol standards compliance

Mail protocol adapters (**IMAP**, **JMAP**, **SMTP**, and **POP3** when implemented) MUST conform to the relevant IETF / JMAP specifications for wire behavior, negotiated capabilities, opaque state, and error handling.

Rules:

- **Specs are the source of truth** — RFC 3501 (+ extensions), RFC 8620/8621, RFC 5321 (+ MIME), RFC 1939 (POP3). Do not invent wire formats or substitute convenience behavior that diverges from the standard.
- **Wire vs display** — Keep server wire forms where the spec requires it (e.g. IMAP mailbox names stay Modified UTF-7 in `external_id`; decode only for UI). Decode RFC 2047 / Modified UTF-7 at the adapter boundary, not in HTTP handlers or the UI.
- **Capability negotiation** — Read `CAPABILITY` / `EHLO` / JMAP session capabilities before emitting commands. Use UID MOVE only when MOVE is advertised; fall back to COPY+EXPUNGE only as an explicit, documented fallback path.
- **Opaque protocol state** — Cursors and tokens round-trip verbatim (`UIDVALIDITY`+UID, JMAP `queryState`, POP3 `UIDL`). The sync engine stores them; adapters own their meaning. Advance cursors only after the batch transaction commits.
- **Spec-defined recovery** — Handle standard edge cases explicitly: IMAP `UIDVALIDITY` change → full folder resync; JMAP `cannotCalculateChanges` → full query; SMTP permanent vs transient failures; POP3 `-ERR` vs `+OK`.
- **Documented exceptions only** — A deliberate deviation from the spec requires a comment in the adapter, an entry in `docs/specs/2026-08-20-lyra-sync-and-protocols-spec.md` §13 (compliance checklist), and a test that proves interop with a reference server.

Compliance tracking lives in sync spec §13. `/api/v1` stays protocol-agnostic; standards apply inside `backend/src/imap.rs`, `smtp.rs`, the JMAP seam (`backend/src/sync/jmap_client.rs`), and future `pop3.rs`.

## HTTP API (client-agnostic)

Lyra’s UI surface is a **versioned HTTP API**. The React app is a peer client, not a privileged front-end. Far-horizon native clients are described in `docs/product/2026-08-20-lyra-multi-client-roadmap.md`; do not build them in v1.

- Expose product capability under **`/api/v1/...`**; avoid web-only backend shortcuts.
- Prefer **OpenAPI** as the public contract as routes land.
- Use **REST** for list/read/mutate; use an explicit event channel (**SSE**, per sync spec) for live sync — not HTML or ad-hoc web-only streams.
- Return stable JSON error shapes `{ "error", "code"? }`; see `docs/specs/2026-08-26-lyra-http-api-surface.md` and `docs/openapi/api-v1.yaml`.
- Auth must work for non-browser clients (token-based Lyra login).
- Breaking changes go to `/api/v2/...`; keep prior versions until clients migrate.

## Verification

- Test through the module interface (sync, protocol adapters, auth), not only UI snapshots.
- Prefer a failing test that proves the bug, then the fix (TDD): for storage seams that means a `postgres_live` roundtrip first, for UI logic a `src/lib/` pure-function test first.
- Form/network decision logic lives in pure functions under `frontend/src/lib/` with colocated vitest files — not inline in components.
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
- Protocol compliance rules or checklist status changes  
