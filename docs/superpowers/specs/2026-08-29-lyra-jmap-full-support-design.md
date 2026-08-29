# Lyra Full JMAP Support — Design

Date: 2026-08-29 · Status: approved (design reviewed in session, user granted autonomous execution)

## Context

Lyra's hand-rolled JMAP client (`backend/src/jmap.rs`) covers a minimal subset of
RFC 8620/8621 and has accumulated functional gaps:

- JMAP attachments are never downloaded (`downloadUrl` not even parsed).
- `Email/queryChanges` `removed` ids are discarded → server-side deletes/moves-out never reflected locally.
- Read/starred flags for JMAP accounts are local-only (`patch_message` only talks IMAP) and are overwritten by the next sync.
- Server-side moves don't update local `folder_id`; `threadId` fetched but never persisted.
- Basic auth only — Fastmail JMAP requires Bearer API tokens, so JMAP is unusable with the canonical JMAP provider.
- Every operation re-discovers the session; one 30s timeout also kills the EventSource stream cyclically.
- No batching beyond one hardcoded pair; `submit_email` does 4 sequential round trips.
- Hand-rolled SSE frame parser; ad-hoc string-matched error classification.
- OpenGPG MIME wrapper silently dropped on JMAP send.

User directive: adopt an open-source crate rather than maintain a private protocol
implementation.

## Crate evaluation

**Chosen: [`jmap-client`](https://github.com/stalwartlabs/jmap-client) v0.4.2** (Stalwart Labs,
Apache-2.0 OR MIT, actively maintained — commits through 2026-06).

- Covers RFC 8620 (core), RFC 8621 (mail), RFC 8887 (WebSocket) + Sieve draft.
- `Credentials::bearer` built in; `event_source` async stream; `blob` upload/download;
  `email_submission`, `identity`, `vacation_response`, `push_subscription`, `thread`,
  `mailbox`, `principal` modules; typed `ProblemDetails` (RFC 7807) / `MethodError` / `SetError`.
- Redirect policy: reqwest `Policy::custom` with a **trusted-host allowlist**
  (default: empty → all redirects error). We keep that default and resolve the
  well-known redirect ourselves before `connect()` (see Security).

Accepted costs:

- reqwest 0.13 joins the tree alongside Lyra's 0.12 (separate 0.x majors). Functional,
  some binary bloat.
- The crate builds a new reqwest `Client` per `send()` (no cross-request connection
  reuse). Acceptable for single-user v1; candidate for an upstream contribution later.
- Trusted-host redirect check is host-based, not origin-based. Neutralized by never
  populating the allowlist (default = deny all) and pre-resolving redirects in our
  own same-origin follower.

## Scope

**In scope — full RFC 8620 + RFC 8621 for a mail client:**

- Core (8620): session capability limits (size batches by `maxCallsInRequest` /
  `maxSizeRequest`), probe = session connect + submission capability check
  (`Core/echo` dropped — jmap-client 0.4.2 cannot emit it), batching + result
  references, typed errors, EventSource push (8620 §7.3).
- Mail (8621): `Mailbox/get|changes|query|set`; `Email/query` (filter model open beyond
  `inMailbox`), `Email/queryChanges` **with `removed` applied**, `Email/changes`,
  `Email/get`, `Email/set` (keywords, mailboxIds, draft create/destroy), `Email/copy`,
  `Email/import` (seam-level), `Thread/get` (**persist `thread_id`**),
  `SearchSnippet/get` (seam-level; search endpoint wiring later), `Identity/get`,
  `EmailSubmission/set|get|query` (batched send + delivery status),
  `VacationResponse/get|set` (seam-level; UI later), blob upload **and download**
  (attachments persisted to the blob store).

**Explicitly out:** WebSocket push (8887), Quotas (9425), `Blob/copy` (9404), Sieve,
PushSubscription (no web-push target), JMAP Contacts/Calendars (Lyra PIM stays on DAV).

## Architecture

Single seam: **`backend/src/sync/jmap_client.rs`** — the only module that imports
`jmap_client`. Everything behind it keeps speaking Lyra's plain DTOs
(`JmapMailbox`, `JmapEmail`, …) so `sync/store.rs` persistence is unchanged.

The seam owns:

1. **Credentials**: `Basic(email, password)` or `Bearer(token)` selected by the
   account's `auth_type` (`"bearer"` is new; token stored in the existing encrypted
   password field — no schema change).
2. **Discovery**: pre-resolve `/.well-known/jmap` redirects with the existing
   same-origin follower (`resolve_discovery_redirect`, landed in `083614a`), then
   `Client::connect()` with the crate default (deny-all redirects).
3. **Session URL origin pinning** post-connect: `apiUrl`/`uploadUrl`/`downloadUrl`/
   `eventSourceUrl` must share the configured origin (`netsec::origin_of`), replacing
   `check_session_urls`.
4. **Session caching** per account (in-process `Mutex<HashMap<account_id, Client>>`
   on the sync state; refresh when `is_session_updated()` reports staleness or on
   401/404 session errors). Kills the per-operation re-discovery.
5. **Type mapping** crate → Lyra DTOs, and Lyra ops → crate requests (batching where
   the spec allows: send = upload + `Email/set` + `EmailSubmission/set` with `#`
   references; sync page = `Email/query` + `Email/get` in one request).

Call sites rewritten (mechanical, each small):

| Call site | Change |
|---|---|
| `sync/jmap_loop.rs` | Sync via seam; apply `removed` ids; persist `thread_id`; batch query+get |
| `sync/send.rs` `deliver_jmap` | Batched upload+create+submit; keep OpenGPG `mime_body` via `Email/import` fallback when MIME-wrapped |
| `sync/http.rs` | Drafts (create/destroy), move/trash/spam (`mailboxIds`), **flags push** (`patch_message` JMAP arm via `Email/set` keywords) |
| `jmap_push.rs` | Crate `event_source` stream; no hand SSE parser; stream-scoped timeout; reconnect backoff stays in supervisor |
| `accounts.rs` | Probe = session connect + submission capability check (`Core/echo` dropped — jmap-client 0.4.2 cannot emit it); `send_protocol` detection unchanged in shape |

Deleted: hand-rolled transport in `jmap.rs` (request tuples, SSE parser, `upload_blob`,
`submit_email`, `discover`). Retained: Lyra DTOs (moved beside the seam),
`resolve_discovery_redirect` (moves into the seam), `netsec.rs` untouched.

## Data flow

Sync: cached session → `Mailbox/get` (+`Mailbox/changes` when cursor exists) → per
mailbox `Email/queryChanges` (apply `removed` → delete local rows; `added` → fetch) or
paged `Email/query` → `Email/get` in chunks batched per capability limits → persist
(`store.rs`, additive only: `thread_id`, attachment rows + blob files, `folder_id`
updates for moved messages).

Send: compose → (attachments: blob upload) → `Email/set` create `$draft` →
`EmailSubmission/set` with `#` back-references + `onSuccessUpdateEmail` — one request
where possible. MIME-wrapped (OpenGPG) outbound uses `Email/import` + submit so the
wrapper survives.

Push: crate EventSource stream (`types=*&closeafter=no&ping=…`) → state change →
enqueue sync job (existing supervisor semantics, incl. backoff).

## Error handling

Map crate `Error` into the existing typed errors with a real transient/permanent
split: transport/timeout → transient (job retry with backoff); `cannotCalculateChanges`
→ cursor reset (replaces today's string matching); `rateLimit`/`overQuota` method
errors → transient with longer backoff; other Method/Set/Problem → permanent.
401 → mark session/auth broken, surface as typed auth error. `jobs.rs` error-message
whitelist sanitization unchanged.

## Testing

- Seam unit tests (hermetic): DTO mapping from crate types, error classification
  matrix, origin pinning of session URLs (malicious cross-origin `apiUrl` rejected),
  redirect resolution (already covered), keyword⇄flag mapping, cursor/removed-ids
  application logic against an in-memory SQLite DB.
- Existing `cargo test --bin lyra_backend` suite must stay green; clippy
  `-D warnings` clean for changed files (pre-existing `oauth/config.rs`
  `result_large_err` on the current toolchain is out of scope; do not add new ones).
- Live acceptance (gated on user-provided Fastmail API token): sync, attachment
  download, flag round-trip, move, draft, send, push event — manual checklist.
- IMAP path untouched; its tests are the regression net.

## Rollout (commit sequence on main)

1. `feat: add jmap-client seam with Bearer auth, session cache, origin pinning`
2. `feat: rewrite JMAP sync loop on seam (removed ids, Email/changes, thread_id)`
3. `feat: download JMAP attachments via blob endpoint`
4. `feat: batched JMAP send with submission status, keep OpenGPG MIME`
5. `feat: JMAP push via crate EventSource stream`
6. `feat: push flags/moves for JMAP accounts; probe via Core/echo` — probe shipped as
   session connect + submission capability check (jmap-client 0.4.2 cannot emit `Core/echo`)
7. `refactor: delete hand-rolled JMAP transport`

Update `AGENTS.md` (stack/notes) and `docs/specs/2026-08-20-lyra-sync-and-protocols-spec.md`
where they describe the old client.

## Risks

- Crate's per-request client rebuild → extra TLS handshakes on bulk sync. Mitigate:
  batch aggressively (capability limits), accept for v1, consider upstream PR.
- reqwest dual stack (0.12 + 0.13) — compile-time only concern.
- Crate type impedance (e.g. keyword sets, address models) — contained inside the seam.
- Fastmail live verification blocked on user-generated API token; hermetic tests carry
  the correctness burden until then.
