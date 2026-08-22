# Plugin kernel + mail loop — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land the approved plugin kernel so add-account → enqueue sync → worker IMAP/JMAP → Inbox → read → send works, with Redis sessions, SQL jobs, and a working snooze control.

**Architecture:** Compile-time plugins register on `App` (Cordis-style inject + lifecycle, not a JS runtime). Receive/send/probe are separate plugin kinds. HTTP enqueues SQL `jobs`; a Tokio worker pool runs them (global IMAP cap + per-account lock). Redis `kv` holds sessions; mail/snooze/jobs stay in SQLite/Postgres.

**Tech Stack:** Rust 2024, Axum 0.8, Tokio, sqlx, `async-trait`, `redis` (tokio), existing `async-imap` / `lettre` / JMAP client, React Settings + `mail-display` snooze UI.

**Spec:** `docs/specs/2026-08-22-lyra-plugin-kernel-spec.md`

## Global Constraints

- Client-agnostic `/api` (no web-only shortcuts); JSON errors; i18n copy in the client (`en` + `zh`).
- Never log secrets or plaintext mail passwords; never commit `.env`, Redis passwords, or `lyra.db`.
- HTTP must not run a full mailbox sync; return after enqueue (`202` or equivalent).
- Sessions/OTP live in `kv` (Redis in Compose; in-memory if `REDIS_URL` unset locally). No `session` table in the mail DB.
- Durable jobs (sync, snooze, send) live in SQL. Redis must not be the mail job broker.
- Unknown `receive_protocol` / `send_protocol` fails closed (no silent IMAP fallback in core).
- Dual migrations: `backend/migrations/sqlite/` **and** `backend/migrations/postgres/`.
- Plugins are compiled in via `builtin_plugins()` — no `.so` loader.
- Before a task is done: `cd backend && cargo test` for touched modules, then `cd backend && cargo fmt` and `cd backend && cargo clippy --all-targets -- -D warnings`. Frontend tasks: `cd frontend && npm run check`.
- Do not implement POP3, Graph/EWS, IMAP IDLE, send-later UI, or email-OTP product flow.
- Map to **five PRs** (spec §10): Tasks 1–3 → PR1; 4–6 → PR2; 7–8 → PR3; 9–10 → PR4; 11 → PR5 (manual proof, no extra code unless bugs).

## File map

| File | Responsibility |
|------|----------------|
| `backend/src/kernel/mod.rs` | Re-exports |
| `backend/src/kernel/app.rs` | `App`, inject check, protocol maps, event bus |
| `backend/src/kernel/plugin.rs` | `Plugin` trait |
| `backend/src/kernel/events.rs` | `AppEvent` |
| `backend/src/protocol/mod.rs` | `ReceivePlugin`, `SendPlugin`, `ReceiveCaps`, `SyncCtx` |
| `backend/src/jobs.rs` | SQL job queue + worker loop |
| `backend/src/kv/mod.rs` | `KvStore` trait |
| `backend/src/kv/memory.rs` | In-memory adapter |
| `backend/src/kv/redis.rs` | Redis adapter |
| `backend/src/scheduler.rs` | 5‑minute poll enqueue |
| `backend/migrations/{sqlite,postgres}/0004_plugin_kernel.up.sql` | receive/send columns, `sess_epoch`, `snoozed_until`, `jobs` |
| `backend/src/main.rs` | `App::boot`, `builtin_plugins`, spawn workers |
| `backend/src/sync.rs` | Look up receive plugin; HTTP enqueue; snooze filter |
| `backend/src/accounts.rs` | Persist receive/send; enqueue sync on create |
| `backend/src/auth.rs` | Sessions via `kv`; epoch on kick |
| `backend/src/config.rs` | `REDIS_URL`, `SYNC_POLL_SECS`, `SYNC_MAX_CONCURRENT` |
| `docker-compose.yml` | Redis service + `REDIS_URL` |
| `frontend/src/components/settings-page.tsx` | Sync button + status |
| `frontend/src/components/mail/mail-display.tsx` | Snooze POSTs |
| `frontend/src/i18n/en.json`, `zh.json` | Sync / snooze strings |

---

### Task 1: Kernel `App` + `Plugin` + fake receive

**Files:**
- Create: `backend/src/kernel/mod.rs`, `plugin.rs`, `app.rs`, `events.rs`
- Create: `backend/src/protocol/mod.rs`
- Modify: `backend/Cargo.toml` (add `async-trait = "0.1"`)
- Modify: `backend/src/main.rs` (add `mod kernel; mod protocol;`)
- Test: tests live in `backend/src/kernel/app.rs` under `#[cfg(test)]`

**Interfaces:**
- Consumes: nothing
- Produces: `App::new()`, `App::register_plugin`, `App::register_receive`, `App::receive(&str)`, `App::emit`, `Plugin`, `ReceivePlugin`, `KernelError::UnknownReceive`, `KernelError::MissingInject`

- [ ] **Step 1: Add `async-trait` and write the failing test**

In `backend/Cargo.toml` under `[dependencies]`:

```toml
async-trait = "0.1"
```

Create `backend/src/kernel/mod.rs`:

```rust
//! Plugin kernel: App, inject, events.
//! See `docs/specs/2026-08-22-lyra-plugin-kernel-spec.md`.

mod app;
mod events;
mod plugin;

pub use app::{App, KernelError};
pub use events::{AppEvent, EventBus};
pub use plugin::Plugin;
```

Create `backend/src/kernel/plugin.rs`:

```rust
use crate::kernel::App;

pub trait Plugin: Send + Sync + 'static {
    fn name(&self) -> &'static str;
    fn inject(&self) -> &'static [&'static str] {
        &[]
    }
    fn register(&self, app: &mut App);
}
```

Create `backend/src/kernel/events.rs`:

```rust
use tokio::sync::broadcast;

#[derive(Debug, Clone)]
pub enum AppEvent {
    SyncStarted { account_id: String },
    SyncComplete { account_id: String },
    SyncError { account_id: String, error: String },
}

#[derive(Clone)]
pub struct EventBus {
    tx: broadcast::Sender<AppEvent>,
}

impl EventBus {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(256);
        Self { tx }
    }

    pub fn emit(&self, event: AppEvent) {
        let _ = self.tx.send(event);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<AppEvent> {
        self.tx.subscribe()
    }
}
```

Create `backend/src/protocol/mod.rs` with the receive trait (object-safe via `async_trait`):

```rust
use async_trait::async_trait;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, Default)]
pub struct ReceiveCaps {
    pub folders: bool,
    pub flags: bool,
    pub push: bool,
    pub delete_on_fetch: bool,
}

pub struct SyncCtx {
    pub account_id: String,
    pub user_id: String,
}

#[derive(Debug, Clone, Default)]
pub struct SyncOutcome {
    pub folders_synced: u32,
    pub messages_synced: u32,
}

#[async_trait]
pub trait ReceivePlugin: Send + Sync {
    fn id(&self) -> &'static str;
    fn capabilities(&self) -> ReceiveCaps {
        ReceiveCaps {
            folders: true,
            flags: true,
            ..ReceiveCaps::default()
        }
    }
    async fn sync_account(&self, ctx: &SyncCtx) -> Result<SyncOutcome, String>;
}

pub type ReceiveHandle = Arc<dyn ReceivePlugin>;
```

Write `backend/src/kernel/app.rs` **only as a stub** so the test compiles against names, then fill in Step 3. First, put this test at the bottom of `app.rs` (the test will fail until `receive` works):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::Plugin;
    use crate::protocol::{ReceiveCaps, ReceivePlugin, SyncCtx, SyncOutcome};
    use async_trait::async_trait;
    use std::sync::Arc;

    struct FakeImap;

    impl Plugin for FakeImap {
        fn name(&self) -> &'static str {
            "imap"
        }
        fn inject(&self) -> &'static [&'static str] {
            &["storage"]
        }
        fn register(&self, app: &mut App) {
            app.provide("storage");
            app.register_receive(Arc::new(FakeImapReceive));
        }
    }

    struct FakeImapReceive;

    #[async_trait]
    impl ReceivePlugin for FakeImapReceive {
        fn id(&self) -> &'static str {
            "imap"
        }
        fn capabilities(&self) -> ReceiveCaps {
            ReceiveCaps {
                folders: true,
                flags: true,
                push: false,
                delete_on_fetch: false,
            }
        }
        async fn sync_account(&self, ctx: &SyncCtx) -> Result<SyncOutcome, String> {
            assert_eq!(ctx.account_id, "acc-1");
            Ok(SyncOutcome {
                folders_synced: 1,
                messages_synced: 2,
            })
        }
    }

    #[tokio::test]
    async fn registers_receive_and_looks_up_by_id() {
        let mut app = App::new();
        FakeImap.register(&mut app);
        let recv = app.receive("imap").expect("imap registered");
        let out = recv
            .sync_account(&SyncCtx {
                account_id: "acc-1".into(),
                user_id: "user-1".into(),
            })
            .await
            .unwrap();
        assert_eq!(out.messages_synced, 2);
    }

    #[test]
    fn unknown_receive_is_error() {
        let app = App::new();
        let err = app.receive("pop3").unwrap_err();
        assert!(matches!(err, KernelError::UnknownReceive(id) if id == "pop3"));
    }

    #[test]
    fn missing_inject_fails_closed() {
        struct NeedsDb;
        impl Plugin for NeedsDb {
            fn name(&self) -> &'static str {
                "needs-db"
            }
            fn inject(&self) -> &'static [&'static str] {
                &["storage"]
            }
            fn register(&self, _app: &mut App) {}
        }
        let mut app = App::new();
        let err = app.register_plugin(&NeedsDb).unwrap_err();
        assert!(matches!(err, KernelError::MissingInject { plugin, service }
            if plugin == "needs-db" && service == "storage"));
    }
}
```

Add `mod kernel;` and `mod protocol;` to `backend/src/main.rs` next to the other `mod` lines.

- [ ] **Step 2: Run the tests — expect compile/fail**

Run: `cd backend && cargo test --lib kernel::app::tests -- --nocapture`

Expected: compile error (`App` / `register_plugin` missing) or FAIL on lookup.

- [ ] **Step 3: Implement `App`**

```rust
use crate::kernel::plugin::Plugin;
use crate::protocol::ReceiveHandle;
use crate::kernel::events::EventBus;
use std::collections::HashMap;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum KernelError {
    #[error("unknown receive protocol '{0}'")]
    UnknownReceive(String),
    #[error("unknown send protocol '{0}'")]
    UnknownSend(String),
    #[error("plugin '{plugin}' injects '{service}' which is not provided")]
    MissingInject { plugin: String, service: String },
}

pub struct App {
    provided: HashMap<&'static str, ()>,
    receive: HashMap<String, ReceiveHandle>,
    pub events: EventBus,
}

impl App {
    pub fn new() -> Self {
        Self {
            provided: HashMap::new(),
            receive: HashMap::new(),
            events: EventBus::new(),
        }
    }

    pub fn provide(&mut self, name: &'static str) {
        self.provided.insert(name, ());
    }

    pub fn register_plugin(&mut self, plugin: &dyn Plugin) -> Result<(), KernelError> {
        for service in plugin.inject() {
            if !self.provided.contains_key(service) {
                return Err(KernelError::MissingInject {
                    plugin: plugin.name().into(),
                    service: (*service).into(),
                });
            }
        }
        plugin.register(self);
        Ok(())
    }

    pub fn register_receive(&mut self, plugin: ReceiveHandle) {
        self.receive.insert(plugin.id().into(), plugin);
    }

    pub fn receive(&self, id: &str) -> Result<ReceiveHandle, KernelError> {
        self.receive
            .get(id)
            .cloned()
            .ok_or_else(|| KernelError::UnknownReceive(id.into()))
    }
}
```

Call `register_plugin` from the first test instead of `FakeImap.register` **after** `app.provide("storage")` **or** change `FakeImap::register` to not require provide-before-register: the missing-inject test uses `register_plugin`; the happy path should `app.provide("storage"); app.register_plugin(&FakeImap)?;`. Update the happy-path test to:

```rust
let mut app = App::new();
app.provide("storage");
app.register_plugin(&FakeImap).unwrap();
```

- [ ] **Step 4: Re-run tests**

Run: `cd backend && cargo test --lib kernel:: -- --nocapture`

Expected: PASS (3 tests).

- [ ] **Step 5: Commit**

```bash
git add backend/Cargo.toml backend/src/kernel backend/src/protocol backend/src/main.rs
git commit -m "$(cat <<'EOF'
feat: add compile-time plugin kernel with receive lookup

EOF
)"
```

---

### Task 2: Send plugin + schema receive/send columns

**Files:**
- Modify: `backend/src/protocol/mod.rs` (add `SendPlugin`)
- Create: `backend/migrations/sqlite/0004_plugin_kernel.up.sql`, `0004_plugin_kernel.down.sql`
- Create: `backend/migrations/postgres/0004_plugin_kernel.up.sql`, `0004_plugin_kernel.down.sql`
- Modify: `backend/src/accounts.rs` (INSERT/SELECT `receive_protocol`, `send_protocol`)
- Modify: kernel tests or `protocol` tests for unknown send id
- Test: `backend/src/kernel/app.rs` (extend) + a sqlx test in `accounts.rs` if one exists; otherwise a focused test in `storage.rs` is not required — account mapping is covered by handler fields

**Interfaces:**
- Consumes: `App` from Task 1
- Produces: `App::register_send`, `App::send(&str) -> Result<SendHandle, KernelError>`, columns `receive_protocol`, `send_protocol`

- [ ] **Step 1: Failing test — unknown send id**

Add to `protocol/mod.rs`:

```rust
#[async_trait]
pub trait SendPlugin: Send + Sync {
    fn id(&self) -> &'static str;
    async fn send(&self, account_id: &str, raw: &str) -> Result<(), String>;
}

pub type SendHandle = Arc<dyn SendPlugin>;
```

Add `send: HashMap<String, SendHandle>` plus `register_send` / `send()` mirroring receive. Test:

```rust
#[test]
fn unknown_send_is_error() {
    let app = App::new();
    let err = app.send("graph").unwrap_err();
    assert!(matches!(err, KernelError::UnknownSend(id) if id == "graph"));
}
```

- [ ] **Step 2: Run test — expect FAIL/compile error until `App::send` exists**

Run: `cd backend && cargo test --lib kernel::app::tests::unknown_send_is_error -- --nocapture`

- [ ] **Step 3: Migration `0004` (sqlite)**

`backend/migrations/sqlite/0004_plugin_kernel.up.sql`:

```sql
ALTER TABLE mail_account ADD COLUMN receive_protocol TEXT NOT NULL DEFAULT 'imap';
ALTER TABLE mail_account ADD COLUMN send_protocol TEXT NOT NULL DEFAULT 'smtp';
UPDATE mail_account SET receive_protocol = protocol WHERE protocol IN ('imap', 'jmap');
UPDATE mail_account SET send_protocol = 'smtp';

ALTER TABLE lyra_user ADD COLUMN sess_epoch INTEGER NOT NULL DEFAULT 0;
ALTER TABLE message ADD COLUMN snoozed_until TEXT;

CREATE TABLE IF NOT EXISTS jobs (
    id TEXT PRIMARY KEY NOT NULL,
    kind TEXT NOT NULL,
    run_at TEXT NOT NULL,
    payload TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending',
    attempts INTEGER NOT NULL DEFAULT 0,
    last_error TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_jobs_due ON jobs(status, run_at);
```

SQLite down: leave a comment that dropping columns is unsupported; `DROP TABLE IF EXISTS jobs;` only.

Postgres `0004` up: same columns with `ALTER TABLE ... ADD COLUMN IF NOT EXISTS`, `jobs` with `TEXT` timestamps (match existing style in `0001_init.up.sql`). Down: `DROP TABLE IF EXISTS jobs;` and drop columns if the file already drops columns that way.

Keep legacy `protocol` column; new writes set all three: `protocol` (compat) + `receive_protocol` + `send_protocol`.

- [ ] **Step 4: `create_account` writes both ids**

When `body.protocol` is `jmap`, set `receive_protocol = "jmap"` else `"imap"`. Always `send_protocol = "smtp"` this cycle. Include the new columns in the INSERT list.

- [ ] **Step 5: Run tests + fmt**

Run: `cd backend && cargo test --lib && cargo fmt && cargo clippy --all-targets -- -D warnings`

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add backend/src/protocol backend/src/kernel backend/src/accounts.rs backend/migrations
git commit -m "$(cat <<'EOF'
feat: split account receive/send protocol ids and add jobs schema

EOF
)"
```

---

### Task 3: Wrap IMAP/JMAP/SMTP as plugins

**Files:**
- Create: `backend/src/plugins/mod.rs`, `imap_receive.rs`, `jmap_receive.rs`, `smtp_send.rs`
- Modify: `backend/src/main.rs` (`builtin_plugins`, `app.provide("storage")`)
- Modify: `backend/src/sync.rs` (`run_account_sync` looks up receive plugin by `receive_protocol`; SMTP send looks up send plugin)
- Test: `backend/src/plugins/mod.rs` tests: registry contains `imap`, `jmap`, `smtp`; looking up `pop3` errors

**Interfaces:**
- Consumes: `ReceivePlugin::sync_account`, `SendPlugin::send`, `App`
- Produces: `ImapReceivePlugin`, `JmapReceivePlugin`, `SmtpSendPlugin`; `fn builtin_plugins() -> Vec<Box<dyn Plugin>>`

- [ ] **Step 1: Failing test — builtin registry**

```rust
#[test]
fn builtin_registers_imap_jmap_smtp() {
    let mut app = App::new();
    app.provide("storage");
    for p in builtin_plugins() {
        app.register_plugin(p.as_ref()).unwrap();
    }
    assert!(app.receive("imap").is_ok());
    assert!(app.receive("jmap").is_ok());
    assert!(app.send("smtp").is_ok());
    assert!(app.receive("pop3").is_err());
}
```

- [ ] **Step 2: Run — FAIL until plugins exist**

Run: `cd backend && cargo test --lib plugins:: -- --nocapture`

- [ ] **Step 3: Implement thin wrappers**

Do **not** rewrite IMAP fetch. Each plugin’s `sync_account` / `send` calls the existing functions:

- `ImapReceivePlugin::sync_account` → existing `crate::sync::run_account_sync` **after** you change `run_account_sync` to take `(db, user_id, account_id)` and **dispatch**:

```rust
let receive_id: String = /* SELECT receive_protocol FROM mail_account */;
let plugin = app.receive(&receive_id)?;
plugin.sync_account(&SyncCtx { account_id, user_id }).await
```

That would recurse if the plugin calls `run_account_sync`. **Avoid recursion:** split today’s body:

- Rename current IMAP loop to `pub(crate) async fn sync_imap(...)` (already `run_imap_sync`).
- Rename JMAP body to `run_jmap_sync` (already exists).
- `ImapReceivePlugin::sync_account` loads the account row and calls `run_imap_sync`.
- `JmapReceivePlugin::sync_account` calls `run_jmap_sync`.
- Core `run_account_sync` **only**: load row → `app.receive(receive_protocol)` → `sync_account`. If `receive_protocol` empty, fall back to legacy `protocol` column for one release.

Pass `Arc<App>` into `AuthState` (new field `pub app: Arc<Mutex<App>>` is wrong — `App` after boot is read-mostly). Use `Arc<App>` with interior `RwLock` only if you must mutate after start. Prefer: finish all `register_*` in `main`, then `let app = Arc::new(app);` and store that. `receive()` only needs `&self`.

`SmtpSendPlugin::send` calls existing `send_message` internals (extract a `pub(crate) async fn deliver_smtp(...)` from the handler).

- [ ] **Step 4: `main` boots plugins**

```rust
let mut app = kernel::App::new();
app.provide("storage");
for plugin in plugins::builtin_plugins() {
    app.register_plugin(plugin.as_ref())?;
}
let app = std::sync::Arc::new(app);
```

Thread `app` into `AuthState` (add field `pub app: Arc<App>`). Update `AuthState::new` signature.

- [ ] **Step 5: Tests + clippy**

Run: `cd backend && cargo test --lib && cargo clippy --all-targets -- -D warnings`

Expected: PASS. Existing `sync.rs` unit tests still pass.

- [ ] **Step 6: Commit**

```bash
git add backend/src/plugins backend/src/sync.rs backend/src/auth.rs backend/src/main.rs backend/src/smtp.rs
git commit -m "$(cat <<'EOF'
feat: register IMAP, JMAP, and SMTP as protocol plugins

EOF
)"
```

---

### Task 4: SQL job queue + worker pool

**Files:**
- Create: `backend/src/jobs.rs`
- Modify: `backend/src/config.rs` (`SYNC_MAX_CONCURRENT`, default `3`)
- Modify: `backend/src/main.rs` (spawn worker)
- Modify: `backend/src/sync.rs` (`trigger_sync` enqueues, does not await IMAP)
- Test: `jobs.rs` `#[cfg(test)]`

**Interfaces:**
- Consumes: `App`, `ReceivePlugin`, `jobs` table
- Produces: `JobPayload::SyncAccount { account_id, user_id }`, `enqueue`, `spawn_workers`, HTTP `202` + `{ "jobId", "status": "queued" }`

- [ ] **Step 1: Failing tests**

```rust
#[tokio::test]
async fn enqueue_then_claim_due_job() { /* insert pending, claim, status=running */ }

#[tokio::test]
async fn second_sync_same_account_skipped_while_running() { /* in-flight set */ }

#[tokio::test]
async fn cursor_not_advanced_on_plugin_error() {
    // Fake receive returns Err; job status=failed; no panic
}
```

Put these in `backend/src/jobs.rs`. Use an in-memory sqlite pool like `sync.rs` tests (`test_pool`). Copy the `test_pool` helper or extract `pub(crate) async fn test_pool()` later if duplication hurts; duplicating 15 lines is OK for this task.

Job payload JSON:

```rust
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum JobPayload {
    SyncAccount { account_id: String, user_id: String },
    UnsnoozeMessage { message_id: String },
    SendMessage { account_id: String, outbound: serde_json::Value },
}
```

- [ ] **Step 2: Run tests — FAIL**

Run: `cd backend && cargo test --lib jobs:: -- --nocapture`

- [ ] **Step 3: Implement queue + workers**

```rust
pub async fn enqueue(
    pool: &sqlx::SqlitePool,
    payload: &JobPayload,
    run_at: &str, // RFC3339 or datetime('now')
) -> Result<String, sqlx::Error>;

pub async fn claim_due(
    pool: &sqlx::SqlitePool,
    now: &str,
) -> Result<Option<ClaimedJob>, sqlx::Error>;
```

Worker: `tokio::spawn` a loop: `claim_due` → dispatch → mark `completed` / `failed`. Hold `tokio::sync::Mutex<HashSet<String>>` for in-flight account ids. Semaphore `Arc<Semaphore>` with `SYNC_MAX_CONCURRENT` permits around `sync_account`.

`trigger_sync` handler: auth → `enqueue(SyncAccount {..}, now)` → `Json` with status queued. **Do not** call `run_account_sync` inline.

On plugin error: `UPDATE jobs SET status='failed', last_error=?, attempts=attempts+1` — `last_error` must not include passwords.

- [ ] **Step 4: Tests pass**

Run: `cd backend && cargo test --lib jobs:: -- --nocapture`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add backend/src/jobs.rs backend/src/sync.rs backend/src/config.rs backend/src/main.rs
git commit -m "$(cat <<'EOF'
feat: enqueue mailbox sync onto a capped worker pool

EOF
)"
```

---

### Task 5: Scheduler — startup + 5‑minute poll

**Files:**
- Create: `backend/src/scheduler.rs`
- Modify: `backend/src/config.rs` (`SYNC_POLL_SECS`, default `300`)
- Modify: `backend/src/main.rs` (start scheduler after workers)
- Modify: `backend/src/accounts.rs` (after successful create, `enqueue` sync)
- Test: `scheduler.rs`

**Interfaces:**
- Consumes: `enqueue`, `jobs`
- Produces: `start_scheduler(app, poll_secs)` ; backoff: on N consecutive failures for an account, next poll delay doubles up to 3600s (store delay in memory `HashMap<account_id, Duration>` — not Redis)

- [ ] **Step 1: Failing test**

```rust
#[tokio::test]
async fn poll_skips_account_already_in_flight() { /* ... */ }

#[tokio::test]
async fn backoff_doubles_after_failures() {
    let mut b = Backoff::default();
    assert_eq!(b.delay("a"), Duration::from_secs(300));
    b.fail("a");
    assert_eq!(b.delay("a"), Duration::from_secs(600));
    b.ok("a");
    assert_eq!(b.delay("a"), Duration::from_secs(300));
}
```

- [ ] **Step 2: Run — FAIL**

Run: `cd backend && cargo test --lib scheduler:: -- --nocapture`

- [ ] **Step 3: Implement**

Tick loop: `tokio::time::interval`. Each tick: `SELECT id, user_id FROM mail_account WHERE is_active=1 AND sync_enabled=1` → enqueue `SyncAccount` if not in-flight. Startup: one immediate tick (do not wait 300s).

Create-account: after INSERT, enqueue sync for that id.

- [ ] **Step 4: Tests pass + clippy**

Run: `cd backend && cargo test --lib scheduler:: jobs:: -- --nocapture`

- [ ] **Step 5: Commit**

```bash
git add backend/src/scheduler.rs backend/src/accounts.rs backend/src/main.rs backend/src/config.rs
git commit -m "$(cat <<'EOF'
feat: poll active accounts every five minutes and sync on create

EOF
)"
```

---

### Task 6: Settings Sync button + status

**Files:**
- Modify: `frontend/src/components/settings-page.tsx`
- Modify: `frontend/src/i18n/en.json`, `zh.json` (`settings.syncNow`, `settings.syncQueued`, `settings.lastSync`, reuse `sync.*`)
- Modify: `backend/src/sync.rs` `sync_status` — `syncing: true` if any job for this user is `running` with kind sync
- Test: `cd frontend && npm run check`

**Interfaces:**
- Consumes: `POST /api/accounts/{id}/sync` → queued; `GET /api/sync/status`
- Produces: per-account Sync button; lastSyncAt display; disable button while that account’s job is running if status API allows; otherwise disable all while `syncing`

- [ ] **Step 1: Add i18n keys**

```json
"syncNow": "Sync now",
"syncQueued": "Sync queued"
```

Chinese: `"立即同步"`, `"已加入同步队列"`.

- [ ] **Step 2: Button handler**

```ts
async function handleSync(id: string) {
  const res = await fetch(`/api/accounts/${id}/sync`, {
    method: 'POST',
    headers: { Authorization: `Bearer ${token}` },
  });
  if (!res.ok) throw new Error('Sync failed');
  window.dispatchEvent(new Event('lyra:sync-complete'));
  await fetchAccounts();
}
```

Do **not** treat 202 as error (`res.ok` is true for 202). After enqueue, poll `GET /api/sync/status` every 2s until `syncing` is false, then `fetchAccounts` and dispatch `lyra:sync-complete` so the mail list reloads.

- [ ] **Step 3: `npm run check`**

Run: `cd frontend && npm run check`

Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add frontend/src/components/settings-page.tsx frontend/src/i18n/en.json frontend/src/i18n/zh.json backend/src/sync.rs
git commit -m "$(cat <<'EOF'
feat: add Settings sync control that enqueues worker jobs

EOF
)"
```

---

### Task 7: `KvStore` trait + memory adapter

**Files:**
- Create: `backend/src/kv/mod.rs`, `memory.rs`
- Modify: `backend/src/auth.rs` (replace `HashMap` session store)
- Test: `kv/memory.rs` and existing `session_store_operations`

**Interfaces:**
- Consumes: `sess_epoch` column
- Produces: `trait KvStore { async fn get/set/del/del_prefix/incr }`, `MemoryKv`, session key `sess:{epoch}:{token}` → `user_id`, pending `pending:{token}`

- [ ] **Step 1: Failing tests**

```rust
#[tokio::test]
async fn set_get_del() { ... }

#[tokio::test]
async fn del_prefix_drops_user_sessions() {
    kv.set("sess:3:aaa", "user-1", None).await.unwrap();
    kv.set("sess:3:bbb", "user-1", None).await.unwrap();
    kv.del_prefix("sess:3:").await.unwrap();
    assert!(kv.get("sess:3:aaa").await.unwrap().is_none());
}

#[tokio::test]
async fn bump_epoch_invalidates_old_tokens() {
    // create session at epoch 0; bump to 1; get_session(old_token) is None
}
```

- [ ] **Step 2: Run — FAIL**

Run: `cd backend && cargo test --lib kv:: -- --nocapture`

- [ ] **Step 3: Implement `MemoryKv` + switch auth**

```rust
#[async_trait]
pub trait KvStore: Send + Sync {
    async fn get(&self, key: &str) -> Result<Option<String>, KvError>;
    async fn set(&self, key: &str, value: &str, ttl_secs: Option<u64>) -> Result<(), KvError>;
    async fn del(&self, key: &str) -> Result<(), KvError>;
    async fn del_prefix(&self, prefix: &str) -> Result<(), KvError>;
}
```

TTL in memory: store `expires_at: Instant` and skip expired on get.

`SessionStore` methods become wrappers over `kv` + `sess_epoch` from DB. `create_session` reads epoch, `SET sess:{epoch}:{token}`. `get_session` tries current epoch only.

`invalidate_user_sessions(pool, kv, user_id)`: `UPDATE lyra_user SET sess_epoch = sess_epoch + 1`; `del_prefix(&format!("sess:{old}:"))` **and** rely on epoch in the key so stale tokens miss even if del_prefix is partial.

- [ ] **Step 4: Tests pass**

Run: `cd backend && cargo test --lib kv:: auth:: -- --nocapture`

- [ ] **Step 5: Commit**

```bash
git add backend/src/kv backend/src/auth.rs
git commit -m "$(cat <<'EOF'
feat: store sessions in a kv plugin with epoch invalidation

EOF
)"
```

---

### Task 8: Redis adapter + Compose

**Files:**
- Create: `backend/src/kv/redis.rs`
- Modify: `backend/Cargo.toml` (`redis` with tokio)
- Modify: `backend/src/config.rs` (`REDIS_URL: Option<String>`)
- Modify: `backend/src/main.rs` (if URL set, Redis kv; else memory + `tracing::warn!`)
- Modify: `docker-compose.yml` (redis service, `REDIS_URL: redis://redis:6379`)
- Test: Redis tests behind `#[ignore]` or skip if no Redis; **always** run memory tests. Add `#[tokio::test] #[ignore = "needs redis"] async fn redis_roundtrip()`

**Interfaces:**
- Consumes: `KvStore`
- Produces: `RedisKv::connect(url)`, production fail-closed if URL set and connect fails (do not silently use memory in production). Detect production as `REDIS_URL` set. If unset → memory.

Use crate: `redis = { version = "0.27", features = ["tokio-comp", "connection-manager"] }` (adjust patch version if 0.27 is yanked; clippy/test will tell you).

- [ ] **Step 1: Failing compile of `RedisKv`**

Implement `get`/`set`/`del`/`del_prefix` (`SCAN` + `DEL`, or Redis `SET` with prefix and `KEYS` only if documented as single-instance — prefer `SCAN`).

- [ ] **Step 2: Boot wiring**

```rust
let kv: Arc<dyn KvStore> = match config.redis_url.as_deref() {
    Some(url) => Arc::new(RedisKv::connect(url).await?),
    None => {
        tracing::warn!("REDIS_URL unset; using in-memory kv (sessions die on restart)");
        Arc::new(MemoryKv::new())
    }
};
```

- [ ] **Step 3: docker-compose**

```yaml
  redis:
    image: redis:7-alpine
    restart: unless-stopped
```

Lyra service: `REDIS_URL: redis://redis:6379` and `depends_on: [redis]`.

Do not commit Redis passwords. Local compose has no AUTH.

- [ ] **Step 4: `cargo test --lib kv::` + clippy**

Expected: memory tests PASS; ignored redis test not run in CI unless you add a redis service later.

- [ ] **Step 5: Commit**

```bash
git add backend/src/kv backend/src/config.rs backend/src/main.rs backend/Cargo.toml docker-compose.yml
git commit -m "$(cat <<'EOF'
feat: add Redis kv adapter for sessions in Compose

EOF
)"
```

---

### Task 9: Snooze job + Inbox filter

**Files:**
- Modify: `backend/src/sync.rs` (list queries skip snoozed; `POST /api/messages/{id}/snooze`)
- Modify: `backend/src/jobs.rs` (dispatch `UnsnoozeMessage`)
- Modify: `frontend/src/components/mail/mail-display.tsx`
- Modify: `frontend/src/i18n/en.json`, `zh.json` if any missing strings
- Test: sqlx test in `sync.rs` or `jobs.rs`

**Interfaces:**
- Consumes: `jobs.enqueue`, `message.snoozed_until`
- Produces: `POST /api/messages/{id}/snooze` body `{ "until": "<RFC3339>" }`; worker clears `snoozed_until` at T

- [ ] **Step 1: Failing tests**

```rust
#[tokio::test]
async fn inbox_hides_snoozed_message() { /* insert message with snoozed_until future; list role=inbox empty */ }

#[tokio::test]
async fn unsnooze_job_clears_column() { /* run dispatch; snoozed_until NULL; list shows row */ }

#[tokio::test]
async fn sync_does_not_clear_snooze() {
    // After upsert_message of same external_id, snoozed_until still set
}
```

`upsert_message` must **not** overwrite `snoozed_until`. Explicitly keep existing value on conflict (add column to INSERT as NULL; `ON CONFLICT` do not update `snoozed_until`).

List SQL add: `AND (m.snoozed_until IS NULL OR m.snoozed_until <= datetime('now'))`

- [ ] **Step 2: Run — FAIL**

Run: `cd backend && cargo test --lib -- snooze -- --nocapture`

- [ ] **Step 3: Handler + worker + UI**

Handler: verify message belongs to user; `UPDATE message SET snoozed_until=?`; `enqueue(UnsnoozeMessage { message_id }, until)`.

Worker: `UPDATE message SET snoozed_until=NULL WHERE id=?`.

`mail-display.tsx`: each preset button `onClick` computes the same dates already shown (`addHours(today, 4)`, etc.) and POSTs. Calendar day: snooze until end of selected day local time (`setHours(18,0,0,0)` is fine). On success, clear selection (`setSelectedMessage(null)`).

- [ ] **Step 4: Tests + `npm run check`**

Run: `cd backend && cargo test --lib -- snooze` and `cd frontend && npm run check`

- [ ] **Step 5: Commit**

```bash
git add backend/src/sync.rs backend/src/jobs.rs frontend/src/components/mail/mail-display.tsx
git commit -m "$(cat <<'EOF'
feat: persist snooze locally and restore mail when the job runs

EOF
)"
```

---

### Task 10: Send path through send plugin (now, not later)

**Files:**
- Modify: `backend/src/sync.rs` `send_message` handler to `app.send(send_protocol)` 
- Modify: `backend/src/plugins/smtp_send.rs`
- Test: existing SMTP unit tests in `smtp.rs` still pass; add a test that unknown `send_protocol` returns 400

**Interfaces:**
- Consumes: `SendPlugin`, `mail_account.send_protocol`
- Produces: compose still `POST /api/messages/send` (synchronous SMTP this cycle is allowed **or** enqueue `SendMessage` with `run_at=now` — prefer enqueue for consistency with workers; handler returns 202 `{ jobId }` **only if** the frontend already treats non-JSON success. **Keep the handler awaiting `SendPlugin::send` if changing to 202 would break `compose-dialog.tsx`.** Check `compose-dialog`: it expects `res.ok` and then success UI. **Await send in the worker but HTTP waits on a oneshot with timeout 30s** is too cute. **Decision: HTTP awaits `SendPlugin::send` (SMTP is one message, not a mailbox).** Enqueue is for snooze/sync only. Document this exception in a comment on the handler.

- [ ] **Step 1: Test unknown send protocol**

```rust
#[tokio::test]
async fn send_rejects_unknown_protocol() {
    // account send_protocol = "graph" → 400
}
```

If spinning a full HTTP server is heavy, unit-test `app.send("graph")` is already Task 2 — here test the handler mapping: load `send_protocol` and call registry.

- [ ] **Step 2: Wire handler to `state.app.send(&send_protocol)`**

- [ ] **Step 3: `cargo test --lib smtp::` and clippy**

- [ ] **Step 4: Commit**

```bash
git add backend/src/sync.rs backend/src/plugins
git commit -m "$(cat <<'EOF'
feat: send mail through the SMTP send plugin

EOF
)"
```

---

### Task 11: Mail-loop proof (manual)

**Files:** none unless a bugfix is required  
**Test:** local run, not CI

- [ ] **Step 1:** `cd backend && cargo run` and `cd frontend && npm run dev`. Compose Redis optional for this proof (`REDIS_URL` unset is OK).
- [ ] **Step 2:** Log in as the local Lyra user. Settings → add a **real** IMAP or JMAP account (probe or manual hosts). Confirm sync job runs (logs `SyncComplete` or Settings last-sync time).
- [ ] **Step 3:** Inbox shows messages; open one; reply/send via compose dialog; snooze one message and confirm it leaves the list.
- [ ] **Step 4:** If anything fails, fix in the owning module (do not add POP3/OAuth). Re-run `cargo test --lib` / `npm run check`.
- [ ] **Step 5:** Commit only if Step 4 produced code. Do not commit `backend/data/lyra.db` or credentials.

---

## Self-review (spec coverage)

| Spec section | Task |
|--------------|------|
| Kernel, inject, events, compile-time plugins | 1 |
| Receive/send split, unknown id fail-closed | 2–3 |
| Opaque cursors / IMAP-JMAP algorithms unchanged | 3 (wrappers) |
| HTTP enqueue, workers, cap, per-account lock | 4 |
| Startup + 5 min poll + backoff | 5 |
| Settings Sync + status | 6 |
| Redis kv, epoch kick, memory fallback | 7–8 |
| SQL jobs, snooze, sync must not clear snooze | 9 |
| Send plugin | 10 |
| Mail loop proof | 11 |
| No POP3/Graph/IDLE/send-later UI/OTP product | honored as out of scope |

**Exception (documented in Task 10):** single-message SMTP stays request-scoped so compose UI does not need a 202 poll. Mailbox sync does not.
