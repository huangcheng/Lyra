# Lyra — Sync Engine & Protocols Spec

**Date:** 2026-08-20  
**Status:** Draft  
**Companion:** Data model (`docs/specs/2026-08-20-lyra-data-model-spec.md`), Engineering standards (`docs/specs/2026-08-20-lyra-engineering-standards.md`)

**Architecture (2026-08-22):** Plugin kernel, workers, jobs/snooze, and Redis kv are specified in `docs/specs/2026-08-22-lyra-plugin-kernel-spec.md`. That document **supersedes** §3.1’s single `MailProtocol` trait (fetch + send on every adapter) and the rule that an account has one `protocol` field. IMAP/JMAP/SMTP **algorithms** in this file still apply.

---

## 1. Overview

The sync engine is Lyra's deepest module. Its job: keep the local database a faithful mirror of one or more remote mail accounts, with the ability to resume after any interruption, and to surface meaningful state to the UI without leaking protocol details.

**Key properties:**

- **Idempotent** — replaying the same sync batch produces the same result.
- **Resumable** — a crash mid-sync resumes from the last committed cursor, not from scratch.
- **Protocol-agnostic at the interface** — the UI and storage layers never know whether JMAP or IMAP is underneath.
- **Observable** — every state change is surfaced to the frontend via typed events.

---

## 2. Protocol preference

| Priority | Protocol | When |
|----------|----------|------|
| 1 | **JMAP** | Server advertises JMAP (via auto-config probe or manual entry) |
| 2 | **IMAP** | JMAP unavailable; IMAP with IDLE for push, polling as fallback |
| 3 | **SMTP** | Send only (not sync); used for outgoing messages regardless of receive protocol |

An account binds **receive** and **send** plugins separately (`receive_protocol` + `send_protocol`). The engine does not mix adapters inside one plugin. See `docs/specs/2026-08-22-lyra-plugin-kernel-spec.md`.

---

## 3. Module seams

```
┌──────────────────────────────────────────────────────┐
│                     sync engine                       │
│                                                       │
│  ┌─────────────┐  ┌─────────────┐  ┌──────────────┐  │
│  │   jmap       │  │   imap       │  │   smtp        │  │
│  │  (adapter)   │  │  (adapter)   │  │  (adapter)    │  │
│  └──────┬──────┘  └──────┬──────┘  └──────┬───────┘  │
│         │                │                 │          │
│         └────────┬───────┘                 │          │
│                  ▼                         │          │
│         ┌───────────────┐                  │          │
│         │  MailProtocol  │◄────────────────┘          │
│         │   (trait)      │                            │
│         └───────┬───────┘                            │
│                 ▼                                     │
│         ┌───────────────┐                             │
│         │  SyncEngine    │                             │
│         │  (orchestrator)│                             │
│         └───────┬───────┘                             │
│                 ▼                                     │
│         ┌───────────────┐                             │
│         │  Storage       │                             │
│         │  (repository)  │                             │
│         └───────────────┘                             │
└──────────────────────────────────────────────────────┘
```

### 3.1 `MailProtocol` trait

The shared interface that JMAP and IMAP adapters implement.

```rust
#[async_trait]
trait MailProtocol: Send + Sync {
    /// Fetch changes since the given cursor. Returns a batch of changes
    /// and a new cursor value. The batch is empty when fully caught up.
    async fn fetch_changes(
        &self,
        folder: &Folder,
        cursor: Option<&SyncCursor>,
    ) -> Result<ChangeBatch, SyncError>;

    /// Fetch full message content for a set of message IDs.
    async fn fetch_messages(
        &self,
        ids: &[&str],
    ) -> Result<Vec<RawMessage>, SyncError>;

    /// List all folders/mailboxes on the server.
    async fn list_folders(&self) -> Result<Vec<RemoteFolder>, SyncError>;

    /// Apply local flag changes to the server (read, starred, deleted).
    async fn apply_flags(
        &self,
        changes: &[FlagChange],
    ) -> Result<(), SyncError>;

    /// Send a message via SMTP (or JMAP submission).
    async fn send(&self, message: &OutboundMessage) -> Result<(), SyncError>;
}
```

### 3.2 `SyncEngine`

Orchestrates the sync loop. Does not know about JMAP or IMAP specifics.

```rust
struct SyncEngine {
    protocol: Box<dyn MailProtocol>,
    storage: Box<dyn MailStorage>,
    event_tx: broadcast::Sender<SyncEvent>,
}
```

Responsibilities:

1. Iterate over all active folders for an account.
2. For each folder, read the stored cursor and call `fetch_changes`.
3. Upsert messages, update folder counts, write the new cursor — all in one transaction.
4. Emit `SyncEvent` variants so the UI can observe progress.
5. Repeat on a timer (poll) or on push notification (IMAP IDLE / JMAP push).

### 3.3 `MailStorage` trait

Repository interface for the database layer.

```rust
#[async_trait]
trait MailStorage: Send + Sync {
    async fn upsert_message(&self, msg: &Message) -> Result<()>;
    async fn upsert_folder(&self, folder: &Folder) -> Result<()>;
    async fn update_cursor(&self, cursor: &SyncCursor) -> Result<()>;
    async fn get_cursor(&self, account_id: &str, folder_id: &str) -> Result<Option<SyncCursor>>;
    async fn mark_deleted(&self, message_id: &str) -> Result<()>;
    async fn update_flags(&self, message_id: &str, flags: &MessageFlags) -> Result<()>;
    // ... more as needed
}
```

Implementations: `SqliteStorage` and `PostgresStorage`. Both behind the same trait; selected at startup from config.

---

## 4. Sync loop detail

### 4.1 Initial sync

1. `list_folders()` → upsert all folders.
2. For each folder, fetch all messages (paginated, e.g. 100 per batch).
3. Write messages and cursor after each page.
4. Emit `SyncEvent::FolderProgress { folder_id, fetched, total }`.
5. On completion: `SyncEvent::FolderComplete { folder_id }`.

### 4.2 Incremental sync

1. Read stored cursor for each folder.
2. `fetch_changes(cursor)` → returns `ChangeBatch { new_ids, updated_ids, deleted_ids, new_cursor }`.
3. Fetch full content for new + updated IDs.
4. Apply in one DB transaction: upsert new, update changed, soft-delete removed.
5. Write new cursor.
6. Emit `SyncEvent::IncrementalComplete { folder_id, changes_count }`.

### 4.3 Idempotency

- Cursors are only advanced **after** the transaction commits.
- Message upserts use `INSERT ... ON CONFLICT UPDATE` (Postgres) or `INSERT OR REPLACE` (SQLite) keyed on `(account_id, external_id)`.
- Replaying the same cursor returns the same `ChangeBatch`; upserting the same messages is a no-op.
- Flag changes are applied with last-write-wins on `updated_at`.

### 4.4 Resume after crash

On startup, the sync engine reads the last committed cursor for each folder and resumes incremental sync from there. No special recovery logic — idempotency handles it.

---

## 5. Auto-config probe

When adding a mail account, Lyra attempts automatic configuration (like Thunderbird / Apple Mail).

### 5.1 Probe order

1. **JMAP:** `GET https://<domain>/.well-known/jmap` → JMAP session resource.
2. **Autoconfig (Mozilla):** `GET https://autoconfig.<domain>/mail/config-v1.1.xml` → IMAP/SMTP settings.
3. **Autodiscover (Microsoft):** `GET https://<domain>/autodiscover/autodiscover.xml` → IMAP/SMTP settings.
4. **SRV records:** DNS SRV lookup for `_imap._tcp.<domain>` and `_submission._tcp.<domain>`.
5. **MX fallback:** Use MX record hostname as the likely mail server.
6. **Manual:** User provides all settings.

Each probe step is tried; if it succeeds, the result is used and further probes are skipped. The `auto_config_source` field records which probe succeeded.

### 5.2 Module

```
backend/src/config_probe.rs
  ├── probe_jmap(domain) -> Option<MailConfig>
  ├── probe_autoconfig(domain) -> Option<MailConfig>
  ├── probe_autodiscover(domain) -> Option<MailConfig>
  ├── probe_srv(domain) -> Option<MailConfig>
  ├── probe_mx(domain) -> Option<MailConfig>
  └── probe(domain) -> MailConfig   // runs all in order
```

All probes are HTTP(S) or DNS calls with timeouts (5s per probe, 30s total). Failures are silent (move to next probe).

---

## 6. JMAP adapter

### 6.1 Capabilities

- Session discovery via `/.well-known/jmap`.
- Mailbox sync (folders).
- Email query + fetch (messages, headers, body parts).
- Email submission (send).
- Flag changes (keywords).
- State-based change tracking (`sinceState`).

### 6.2 Sync flow

1. Authenticate (Bearer token or Basic auth, depending on server).
2. Get current account state.
3. For each mailbox: `Email/query` with `sinceState` to get changed IDs.
4. `Email/get` for new/changed emails (properties: headers, bodyValues, keywords).
5. Update local DB and state token.

### 6.3 Push

JMAP servers may support `EventSource` push. The JMAP adapter listens for push events and triggers an incremental sync on change, reducing polling.

---

## 7. IMAP adapter

### 7.1 Capabilities

- `LOGIN` / `AUTHENTICATE` (PLAIN, XOAUTH2).
- `LIST` / `LSUB` for folder discovery.
- `FETCH` for message retrieval (BODY[], ENVELOPE, FLAGS, UID).
- `STORE` for flag changes.
- `UID COPY` / `UID MOVE` / `UID EXPUNGE` for folder operations.
- `IDLE` for push notifications (optional).

### 7.2 Sync flow

1. Connect, authenticate, `LIST` folders.
2. `SELECT` each folder.
3. Compare `UIDVALIDITY` with stored value.
   - If changed: full re-sync of that folder.
4. `UID SEARCH` for UIDs > last known UID (or `HIGHESTMODSEQ` if supported).
5. `FETCH` headers + flags for new UIDs.
6. `FETCH BODY[]` on demand (lazy body download for large messages).
7. Store updated cursor: `(uidvalidity, highest_uid_or_modseq)`.

### 7.3 IDLE

When IDLE is supported, the IMAP adapter enters IDLE mode after completing sync. On `EXISTS` notification, it exits IDLE and runs an incremental sync.

---

## 8. SMTP adapter (send)

### 8.1 Capabilities

- `EHLO`, `STARTTLS`, `AUTH` (PLAIN, LOGIN, XOAUTH2).
- `MAIL FROM`, `RCPT TO`, `DATA`.
- Support for 8-bit MIME and UTF-8 addresses.
- DSN (Delivery Status Notification) if the server supports it.

### 8.2 Module

```
backend/src/smtp.rs
  struct SmtpAdapter { ... }
  impl MailProtocol for SmtpAdapter {
      async fn send(&self, message: &OutboundMessage) -> Result<(), SyncError>;
      // other methods return ProtocolUnsupported for send-only adapter
  }
```

Outbound messages are stored in a local `drafts` or `outbox` folder before sending, and moved to `sent` after successful SMTP delivery.

---

## 9. UI observation layer

The frontend never calls the sync engine directly. Instead, it observes state via three channels, each with a clear role.

### 9.1 State roles (no overlap)

| Layer | Library | Owns | Example |
|-------|---------|------|---------|
| **Data** | **Zustand** | Normalised mail data (messages, folders, threads, accounts) | `useMailStore.getState().messages` |
| **Flows** | **XState** | Multi-step UI flows (login + TOTP) | `authMachine` |
| **Async / recovery** | **RxJS** | Long-lived subscriptions to sync events, retry logic, backpressure | `syncEvent$.pipe(retry(3))` |

### 9.2 Sync events → frontend

The backend emits `SyncEvent` via Server-Sent Events (SSE) at `/api/v1/sync/events` (bearer auth; `EventSource` cannot set Authorization — the web client uses `fetch`):

```typescript
type SyncEvent =
  | { type: 'sync_started'; accountId: string }
  | { type: 'folder_progress'; accountId: string; folderId: string; fetched: number; total: number }
  | { type: 'folder_complete'; accountId: string; folderId: string }
  | { type: 'incremental_complete'; accountId: string; folderId: string; changes: number }
  | { type: 'sync_error'; accountId: string; error: string }
  | { type: 'sync_complete'; accountId: string }
```

**RxJS** subscribes to the SSE stream and:

1. Retries on connection drop (exponential backoff).
2. Buffers rapid events (backpressure).
3. Pushes normalised data into **Zustand** store slices on `sync_complete` / `sync_error` (folder + account refresh).

### 9.3 XState machines (v1)

| Machine | States | Purpose |
|---------|--------|---------|
| `authMachine` | `checkingStatus → idle/bootstrap/login/totpChallenge → authenticated` | Lyra login + optional TOTP |

### 9.4 Zustand slices (v1)

| Slice | Data |
|-------|------|
| `mailSlice` | Messages, threads, folders (normalised by ID) |
| `accountSlice` | Account metadata (no credentials) |
| `uiSlice` | Selected folder, selected message, compose state, search query, language |

---

## 10. Failure modes and recovery

| Failure | Where | Recovery |
|---------|-------|----------|
| Network drop mid-sync | Protocol adapter | Sync engine catches error, emits `sync_error`, retries from last cursor on next tick |
| Auth token expired | JMAP/IMAP adapter | Adapter refreshes token (if OAuth2) or re-authenticates; retries operation |
| IMAP UIDVALIDITY change | IMAP adapter | Full re-sync of affected folder; cursor reset |
| Server returns partial data | Protocol adapter | Adapter retries the failed batch; cursor not advanced |
| Database write fails | Storage | Transaction rolls back; cursor unchanged; sync retries |
| Message too large | Protocol adapter | Skip message, log warning, continue; mark message as `fetch_error` in DB |
| Credential decrypt fails | Auth/storage | Account marked inactive; UI prompts for re-entry |
| SSE connection drops | Frontend (RxJS) | Automatic reconnect with exponential backoff (1s → 2s → 4s → … → 60s cap) |
| XState machine stuck | Frontend | Machine has timeout transitions; `error` state offers retry/reset |

Every failure mode produces a typed error at the module boundary. No `catch` blocks swallow errors silently. No secrets appear in error messages.

---

## 11. Background sync scheduling

- **Polling interval:** Configurable per account, default 5 minutes.
- **IDLE / push:** When the protocol supports it (IMAP IDLE, JMAP EventSource), sync is triggered by server push rather than polling.
- **Backoff:** On repeated failures, the poll interval doubles up to a cap (1 hour). On success, it resets to the configured default.
- **Startup:** All active accounts sync immediately on backend start.
- **Shutdown:** In-progress syncs are given a grace period (10s) to commit current state before forced exit.

---

## 12. Explicit non-goals

| Non-goal | Reason |
|----------|--------|
| Sending via JMAP email submission + SMTP simultaneously | One send path per account |
| Conflict resolution for simultaneous edits | Single-user v1; last-write-wins |
| Server-side search delegation | Local search only in v1; server search can be added later |
| Partial message fetch with on-demand download in v1 | Full fetch on sync; lazy body download is a v2 optimisation |
| Sync across multiple instances | Single-instance v1; database is the lock |

---

## Implementation notes (as of 2026-08-23)

These match the running tree; older bullets above that still mention stubs are historical.

| Topic | Implementation |
|-------|----------------|
| HTTP surface | Product API is **`/api/v1/...`**. `/health` and `/version` are unversioned. |
| Credentials | `LYRA_MASTER_KEY` (32+ bytes) → per-user KEK (HKDF) → wrapped DEK in `lyra_user.encrypted_dek`. Account passwords and TOTP secrets encrypt under the DEK. No `SESSION_SECRET`; sessions are bearer tokens in kv. |
| MIME / HTML | IMAP bodies parsed with **mail-parser**; HTML sanitized with **ammonia** at persist (`persist_body_html`). |
| Sync writes | Per folder page: upserts, folder counts, and cursor commit in **one DB transaction**. Cursor advances only after commit. |
| JMAP cursor | Stored `queryState` is sent as `sinceQueryState` on `Email/queryChanges`; `cannotCalculateChanges` clears the cursor and falls back to a full `Email/query`. |
| Postgres | Dual-DB query macros rewrite SQLite SQL; UUID / timestamptz / jsonb bound natively. |
| Sync module | `backend/src/sync/` (`http`, `store`, `imap_loop`, `jmap_loop`, `send`, `types`) — not a single `sync.rs`. |
| Account setup | Settings page probe + form. There is no `accountSetupMachine`. |
| Frontend client | `frontend/src/lib/api-client.ts` injects the bearer token and maps session-expiry 401s to login. |

---

## Related docs

- Product spec: `docs/product/2026-08-20-lyra-v1-product-spec.md`
- Engineering standards: `docs/specs/2026-08-20-lyra-engineering-standards.md`
- Data model: `docs/specs/2026-08-20-lyra-data-model-spec.md`
