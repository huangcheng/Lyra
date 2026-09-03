# Account Sync Error Log (Operator Detail) — Design

Date: 2026-09-03  
Status: Approved  
Choice: Option 2 — self-hosted operator detail (scrubbed cause chain)

## Problem

Settings → Accounts shows only the sanitized SSE category (`同步失败: IMAP error`).
The actionable cause (`TLS error: unexpected EOF`, etc.) lives in server logs only.

## Decision

Persist a **scrubbed** cause chain alongside the category whitelist, and expose a
per-account history on the accounts page for investigation. Never send raw
credentials, tokens, or password substrings to the client.

## Storage

- Migration: add `jobs.last_error_detail TEXT NULL` (SQLite + PostgreSQL).
- On sync job failure:
  - `last_error` ← existing `sanitize_error` category
  - `last_error_detail` ← scrubbed `error_chain`, capped (~2KB)
- On success: clear both (existing `mark_completed` clears `last_error`; also clear detail).

## Scrubbing

Strip (case-insensitive): `password=…`, `Bearer …`, long hex/base64 token-like
runs, credential JSON blobs. **Keep** hostnames and protocol phrases
(`unexpected EOF`, `TLS error`, IMAP/JMAP chatter without secrets).

## API

`GET /api/v1/accounts/{account_id}/sync-errors?limit=20`

Response:

```json
{
  "items": [
    {
      "id": "job-id",
      "at": "2026-09-03T04:00:00Z",
      "category": "IMAP error",
      "detail": "imap error: TLS error: unexpected EOF",
      "attempts": 2
    }
  ]
}
```

Auth: session required; account must belong to the user. Only `SyncAccount`
jobs for that account with `status = failed` (and non-null category).

SSE `sync_error` remains category-only.

## UI

Settings → Accounts: a quiet text control **View error log** opens a **modal**
(Radix Dialog — same chrome as other Lyra dialogs), not an inline expand panel.

Modal contents:
- Title: Error log · `{email}`
- Scrollable list (newest first): time, category, attempts; monospace scrubbed
  `detail` when present
- When `detail` is null: muted note that no cause was recorded (pre–detail
  logging / category-only failures)
- Loading / fetch-error / empty states inside the modal
- Account row actions stay aligned with the header row (no layout stretch)

SSE `sync_error` remains category-only on the account card.

When an IMAP session is **poisoned** mid-pass (parse/io/timeout), the sync
job is marked **failed** (partial folder progress already persisted) so the
error log receives scrubbed detail. Soft per-folder skips that leave the
session healthy still complete successfully without a log row.

## Out of scope

Unscrubbed chains, docker log tailing, cross-account admin views, changing the
category whitelist semantics for `jobs.last_error`.
