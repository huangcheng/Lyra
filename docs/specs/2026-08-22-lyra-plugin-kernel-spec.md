# Lyra — Plugin Kernel, Workers, and Mail Loop

**Date:** 2026-08-22  
**Status:** Draft (awaiting review)  
**Companion:** Product (`docs/product/2026-08-20-lyra-v1-product-spec.md`), sync/protocols (`docs/specs/2026-08-20-lyra-sync-and-protocols-spec.md`), data model (`docs/specs/2026-08-20-lyra-data-model-spec.md`), engineering standards (`docs/specs/2026-08-20-lyra-engineering-standards.md`), UI (`docs/specs/2026-08-21-lyra-shadcn-mail-ui.md`)

This document is the architecture for growing Lyra without turning `sync.rs` / `main.rs` into god-files. IMAP/JMAP/SMTP **algorithms** stay in the sync spec; **how plugins register, run, and are scheduled** lives here.

Ideas taken from [Cordis](https://github.com/cordiverse/cordis): a small context, services, explicit inject, lifecycle, events. Not copied: JS Proxy, Fiber HMR, isolate/intercept, YAML plugin loader, dynamic `.so` loading.

---

## 1. Goal of this cycle (mail loop)

Close the end-to-end mail path on the new kernel:

1. Add account (probe + manual hosts) → persist receive + send plugin ids.  
2. Enqueue sync (do not run a full mailbox fetch inside the HTTP handler).  
3. Worker syncs → Inbox fills → open a message → compose / reply / send.  
4. Settings **Sync** button enqueues the same job.  
5. Background poll every 5 minutes per active account.  
6. **Snooze** (existing reading-pane control) becomes a real delayed job.

**Out of this cycle:** POP3 implementation, Graph/EWS/OAuth, IMAP IDLE / JMAP push, send-later UI, email-OTP product flow, CardDAV/CalDAV polish, attachments/search in the reading pane, a separate `lyra-worker` binary.

---

## 2. Kernel

```
App (kernel)
  ├── Plugin registry     name + inject + lifecycle
  ├── Service map         typed slots (storage, protocols, kv, jobs, scheduler, …)
  ├── Event bus           SyncStarted, SyncComplete, SyncError, AccountAdded, …
  └── Http mounts         each plugin adds its own /api routes
           ▲
           │ register()
  auth · storage · kv · jobs · imap · jmap · smtp · probe · scheduler · pim · (pop3 later)
```

### 2.1 Plugin shape

A plugin is a compiled Rust module. `main` calls `builtin_plugins()` at startup. Adding a function is a new module + a register line, not a rewrite of the engine.

| Hook | When | May do I/O? |
|------|------|-------------|
| `name` | always | no |
| `inject` | declared deps (`storage`, `kv`, `crypto`, …) | no — missing inject fails **startup** |
| `register(&mut App)` | boot | no — factories and routes only |
| `start` / `stop` | after all registers / on shutdown | yes — scheduler loop, connection pools |

Core code never `match protocol`. It asks the registry (`app.receive("imap")`, `app.send("smtp")`, `app.kv()`, `app.jobs.enqueue(...)`).

### 2.2 What we do not build

- Dynamic library loading, a JS plugin sandbox, Cordis Fiber/HMR.  
- Frontend plugin runtime — UI stays compiled feature modules; `/api` is the surface.  
- Hypothetical seams with only one adapter, except **kv** (memory for tests, Redis in production) and **jobs** (SQL now; broker later if we multi-process).

### 2.3 Plugin kinds (v1)

| Kind | Job | First adapters |
|------|-----|----------------|
| **Receive** | folders, fetch since cursor, flags | `imap`, `jmap` (`pop3` later) |
| **Send** | outbound | `smtp` (JMAP submission later) |
| **Probe** | guess hosts from email domain | Mozilla ISPDB, common patterns (SRV/autodiscover later) |
| **Scheduler** | when work runs | startup, 5‑minute poll, job due-scan |
| **Jobs** | durable delayed/background work | SQL table + worker pool |
| **Kv** | ephemeral auth state | Redis (Compose); in-memory (tests / local without Redis) |

---

## 3. Accounts and protocols

An account stores **two** ids (not a single `mail_account.protocol`):

- `receive_protocol`: `imap` | `jmap` | (later `pop3`)  
- `send_protocol`: `smtp` | (later `jmap`)

That is how POP3+SMTP and JMAP-only both fit. Keep existing `imap_*` / `smtp_*` / `jmap_base_url` columns; stop using a combined `protocol` as the source of truth (migrate: `jmap` → receive `jmap` + send `smtp` unless JMAP submission exists).

Each receive plugin declares capabilities so the engine does not pretend POP3 is IMAP:

| Capability | IMAP / JMAP | POP3 (later) |
|------------|-------------|--------------|
| folders | yes | single Inbox |
| server-side flags | yes | limited |
| push (IDLE / EventSource) | later | no |
| delete-on-fetch | no | optional |

Cursors are **opaque bytes** owned by the plugin (IMAP `UIDVALIDITY+UID`, JMAP `state`, later POP3 `UIDL`). The engine only stores and returns them. Cursor advances **after** the batch transaction commits.

Send is a separate path: compose → `send_plugin.send(...)`. Receive plugins do not implement send; send plugins do not implement fetch.

Unknown `receive_protocol` / `send_protocol` is a hard error (no silent IMAP fallback). JMAP-then-IMAP inside one account remains an **implementation detail of the jmap receive plugin**, not a core `match`.

---

## 4. One sync run

Protocol-blind. For one account:

1. Resolve receive (and send only if the job is outbound).  
2. Emit `SyncStarted`.  
3. `receive.list_folders()` → upsert. If `folders: false`, treat as one Inbox.  
4. Per folder: load cursor → `fetch_changes` → upsert messages/flags/deletes → commit → save cursor.  
5. Update unread counts and `last_sync_at`. Emit `SyncComplete` or `SyncError` (typed, no secrets).

**Triggers**

| Trigger | Behavior |
|---------|----------|
| Account saved | Enqueue `SyncAccount { id }` |
| Settings Sync | Enqueue one account or all active |
| Backend start | Enqueue each active account |
| Every 5 minutes | Per-account poll job; skip if in-flight |

Per-account mutex: a second trigger while running **no-ops** (or returns `already_syncing`). On repeated failure, poll interval doubles up to 1 hour; success resets to 5 minutes.

UI: Settings shows last sync time, Sync button, status `idle | syncing | error`. Mail list reloads on `SyncComplete`. Empty Inbox after a successful sync is real data.

IMAP IDLE / JMAP EventSource are future capabilities on the same receive plugin, not extra engines.

---

## 5. Jobs (durable) vs kv (ephemeral)

### 5.1 Jobs — SQL, not Redis

Snooze and send-later must survive Redis flush and process restart.

Table sketch: `kind`, `run_at`, `payload`, `status`, `attempts`, plus account/message ids as needed.

| `kind` | At `run_at` | This cycle |
|--------|-------------|------------|
| `sync_account` | now / poll | yes |
| `unsnooze_message` | user-chosen time | yes (wire existing snooze UI) |
| `send_message` | optional `send_at` | send **now** yes; send-later **UI** later |

Snooze is **local**: set `snoozed_until` on the message, Inbox queries skip those rows, job clears it at T. IMAP has no standard snooze. Incremental sync must not force a snoozed message back into Inbox visibility.

Compose “send now” still goes through outbox → `SendPlugin` → sent (sync spec §8). Same jobs table can hold `send_message` with `run_at = now`.

### 5.2 Kv — Redis for sessions and short-lived codes

Do **not** store sessions in the mail database (backups, migrations, bloat). Password change must kick every session in a few operations.

| Plane | Store |
|-------|--------|
| Mail, folders, jobs, snooze | Primary DB (SQLite / Postgres) |
| Sessions, pending TOTP tokens, OTP/verify codes, rate limits | Redis via `kv` |

Password change: increment `sess_epoch` (or equivalent) on the user and delete `sess:{user_id}:*` (or include epoch in the key so old tokens miss). Epoch stays on the user row so a missed key cannot outlive a password change.

Compose **ships Redis**. Tests use in-memory `kv`. Local `cargo run` may use in-memory `kv` if `REDIS_URL` is unset (sessions die on restart; log a warning). Production without Redis is not a supported session backend.

Email verification **product flow** (reset mail, invite codes) is **not** in this cycle. The kv key shape (`verify:{id}` + TTL) is reserved so that flow can land without a new store.

---

## 6. Workers and threads

Tokio is already multi-thread. That is not enough: full mailbox sync must not run inside the HTTP handler.

| Kind | Examples | Where |
|------|----------|--------|
| Async I/O | IMAP/JMAP, SMTP, HTTP, Redis | Tokio tasks |
| CPU | Argon2, MIME parse, later search index | `spawn_blocking` / small CPU pool |
| Jobs | account sync, unsnooze, send | Worker pool pulling `jobs` |

```
HTTP (Axum)  →  enqueue job, return quickly
                    │
                    ▼
              Job workers (N Tokio tasks)
                    │
         ┌──────────┼──────────┐
         ▼          ▼          ▼
      Receive    SendPlugin   spawn_blocking
      (IMAP)     (SMTP)       (CPU)
```

Rules:

- HTTP enqueues and returns; workers emit sync events.  
- Per-account lock **plus** a global cap (default 2–4 concurrent receive syncs).  
- Same process for v1 (one Docker service). A later `lyra-worker` binary is the same plugins, different `main`.  
- No unbounded `std::thread::spawn`. IMAP stays async.  
- Redis is not the mail job broker. Optional Redis pub/sub (or a list) may **wake** workers so they do not wait the full tick; the source of truth remains SQL `jobs`.

---

## 7. Failure modes

| Failure | Recovery |
|---------|----------|
| Wrong password / auth | `AuthFailed`; account stays saved; UI reconnect; poller backs off |
| Network drop mid-folder | Cursor not advanced; next run resumes; `SyncError` |
| Unknown plugin id | Fail closed at startup or job start |
| Decrypt fails | Account inactive; prompt for password; never log plaintext |
| One account fails | Isolated; other accounts continue |
| Sync already running | No-op / `already_syncing` |
| Probe finds nothing | Not fatal; user fills hosts |
| Redis down (production) | Auth fails closed; mail DB still readable after existing sessions expire — do not fall back to stuffing sessions into SQLite silently |
| Job worker panic | Job stays `pending`/`failed`; retry with backoff; cursor rules unchanged |

HTTP errors: stable JSON shape; user-facing copy in client i18n.

---

## 8. Tests (kernel seams)

1. Fake `ReceivePlugin` + `SendPlugin` on `App` — upserts; cursor advances only after commit.  
2. Crash mid-batch — replay does not duplicate; cursor unchanged until success.  
3. Unknown protocol id — hard error.  
4. Scheduler skips in-flight; backoff after N failures.  
5. Snooze: hidden from Inbox until `run_at`; job restores visibility; sync does not undo snooze.  
6. Password change invalidates all `kv` sessions for that user.  
7. Existing IMAP/JMAP unit tests still pass after wrap-as-plugin.  
8. HTTP sync endpoint returns without waiting for IMAP (job enqueued).

---

## 9. Schema deltas (this cycle)

Relative to `docs/specs/2026-08-20-lyra-data-model-spec.md`:

- `mail_account.receive_protocol`, `mail_account.send_protocol` (migrate from `protocol`).  
- `lyra_user.sess_epoch` (integer, bump on password change).  
- `message.snoozed_until` (nullable timestamp).  
- `jobs` table as in §5.1.  
- No `session` table in the mail DB.

---

## 10. Implementation order (same spec, multiple PRs)

Do not land the whole document in one diff.

1. **Kernel + wrap** — `App`, plugin trait, register IMAP/JMAP/SMTP/probe; split receive/send columns; tests with fakes.  
2. **Workers + scheduler** — enqueue sync from HTTP/save/startup; 5‑minute poll; global cap; Settings status + Sync button.  
3. **Kv** — Redis sessions; `sess_epoch` on password change; in-memory adapter for tests.  
4. **Jobs + snooze** — `jobs` table; wire snooze popover; Inbox filter; worker unsnooze.  
5. **Mail loop proof** — add a real IMAP/JMAP account, sync, read, reply/send.

---

## 11. Related docs

- Sync algorithms (IMAP UID, JMAP state, SMTP, probe order, SSE event types): `docs/specs/2026-08-20-lyra-sync-and-protocols-spec.md` — **§3.1 god `MailProtocol` trait and “one protocol field per account” are superseded by this document.**  
- Dual-DB types and mail tables: `docs/specs/2026-08-20-lyra-data-model-spec.md`  
- Product UI: `docs/specs/2026-08-21-lyra-shadcn-mail-ui.md`
