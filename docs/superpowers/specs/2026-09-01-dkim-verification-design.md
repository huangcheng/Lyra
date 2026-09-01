# DKIM verification — design

**Date:** 2026-09-01
**Status:** Approved (design), pending implementation plan

## Goal

Verify DKIM signatures on incoming mail and surface the result in the
reading pane (Thunderbird "DKIM Verifier" parity): a status line per message
— valid / invalid / not signed — with a details popover (SDID, AUID,
selector, algorithm, signed headers, warnings, dates).

Confirmed scope decisions:

- Existing synced mail gets verdicts via **lazy verify on open** (one raw
  refetch per message, result stored), not bulk backfill.
- The status line shows for **all three states**, including "not signed".

## Current state

- Lyra stores **no raw MIME**: `message` rows hold parsed headers and bodies
  only (`backend/src/entities/message.rs`). Exact bytes exist in memory only
  at ingest — IMAP fetches `BODY.PEEK[]` (`backend/src/imap.rs:493`), JMAP
  downloads the blob. Verification must therefore happen at ingest; old mail
  needs a refetch to verify.
- No DKIM columns on `message`; no auth-results parsing anywhere.
- Companion spec: `2026-09-01-sender-avatars-design.md` (BIMI gate upgrades
  to use DKIM alignment once this lands — follow-up, not blocking).

## Design

### Engine

`mail-auth` (Stalwart Labs) — the standard Rust DKIM/SPF/DMARC crate, same
ecosystem as our `jmap-client`. Used for **verification only**: relaxed/
simple canonicalization, DNS key lookup, RSA and Ed25519. Its DNS resolver
runs on the backend; lookups are subject to the same operational posture as
the media pipeline (timeouts, no user data in queries beyond the selector
domain, which the sender published publicly anyway).

### Ingest-time verification

In the sync persist path (IMAP loop and JMAP loop), while the raw bytes are
in memory, run `mail-auth` DKIM verification over the exact byte stream and
store the outcome on the message row. Verification failure of the *process*
(DNS down, timeout) stores `temperror` — eligible for lazy re-verify — and
never fails or delays the sync itself.

Multiple `DKIM-Signature` headers: evaluate all, store the best by this
order — (1) passing signature whose d= aligns with the From domain,
(2) any passing signature, (3) the first signature's result.

### Stored columns (dual-DB migration, `message` table)

| Column | Type | Content |
|--------|------|---------|
| `dkim_status` | TEXT NULL | `pass` / `fail` / `none` / `temperror` (NULL = never verified) |
| `dkim_sdid` | TEXT NULL | d= signing domain |
| `dkim_auid` | TEXT NULL | i= agent/user identity |
| `dkim_selector` | TEXT NULL | s= selector |
| `dkim_algorithm` | TEXT NULL | e.g. `rsa-sha256`, with key bits when known (`RSA 1024 / SHA-256`) |
| `dkim_signed_headers` | TEXT NULL | h= list, comma-separated |
| `dkim_warnings` | TEXT NULL | e.g. `Header 'Subject' is not signed` (JSON array of strings) |
| `dkim_signed_at` | TIMESTAMP NULL | t= tag |
| `dkim_expires_at` | TIMESTAMP NULL | x= tag |

### Lazy verify on open

`GET /api/v1/messages/{id}`: when `dkim_status` is NULL or `temperror`, the
handler refetches the raw message — IMAP: `UID FETCH … BODY.PEEK[]`; JMAP:
blob download by email id — verifies inline (bounded: skip when
`size_bytes` > 10MB; 5s overall budget), stores the verdict, and includes it
in the response. On refetch/verify failure the message serves normally
without a verdict; the row stays NULL so a later open retries.

### API shape

Message detail payloads gain:

```json
"dkim": {
  "status": "pass",
  "sdid": "duck.com",
  "auid": "@duck.com",
  "selector": "dkim",
  "algorithm": "RSA 2048 / SHA-256",
  "signedHeaders": ["date", "from", "to", "subject"],
  "warnings": [],
  "signedAt": "2026-08-30T10:59:00Z",
  "expiresAt": null
}
```

`"dkim": null` when never verified. List payloads omit it (verification
costs a refetch for old mail; the status line lives in the reading pane).

### Frontend

- `MessageCard` (expanded state): a status line above the body —
  `DKIM Valid (Signed by {sdid})` (green check), `DKIM Invalid (E-Mail was
  modified)` (red), `Not signed` (neutral) — clickable, opening a popover
  with the full detail set (SDID, AUID, selector, algorithm, signed headers,
  warnings, sign/expiration dates).
- en + zh i18n keys for all three states and popover labels.
- Pure label/formatting helpers in `frontend/src/lib/` with colocated tests.

### Error handling

- Verification never blocks rendering, sync, or message serving.
- `temperror` (DNS/timeout at ingest) is retried via the lazy path; it is
  displayed as no status (not as "invalid").
- Mailing-list modifications legitimately produce `fail` — the UI copy says
  "modified", not "forged".

## Testing

- Backend unit tests with known DKIM test vectors (mail-auth ships signed
  test messages; add our own for: pass, body-modified fail, unsigned,
  expired signature, key revoked in DNS (`p=` empty)):
  - signature selection policy (aligned pass beats unaligned pass beats
    fail);
  - ingest path stores verdict for both IMAP and JMAP persist flows (table
    roundtrip on SQLite + `postgres_live` per repo convention);
  - lazy path: NULL row → refetch → verdict stored → included in response;
    refetch failure → response without verdict, row stays NULL.
- Frontend: status-line label helper (all three states, both locales),
  popover field formatting.
- No live DNS/network in tests — inject mail-auth's resolver behind a seam.

## Out of scope (YAGNI)

- SPF (impossible for a client — we never see the connecting IP) and DMARC
  enforcement; Authentication-Results header parsing.
- ARC.
- DKIM **signing** of outgoing mail (the sending server's job).
- Scoring, filtering, or blocking based on verdicts — display only.
- Async/deferred lazy verification (inline with a time budget is the spec).
