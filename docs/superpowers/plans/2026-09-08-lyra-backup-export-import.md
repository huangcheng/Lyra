# Lyra Full-Instance Backup (Export/Import) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Password-encrypted full-instance backup (mail + accounts w/ credentials + settings + contacts + calendars) as a single `.lyra` file, with additive-merge import.

**Architecture:** New `backend/src/backup/` module (format / crypto / export / import / http). Export and import run as new `JobPayload` variants on the existing `jobs.rs` worker pool; progress, reports, and the artifact registry live in kv (the jobs table has no result columns). Archive = zip staged on disk, then age passphrase-encrypted. Import uploads in 8 MiB chunks (Cloudflare 100 MB cap). Raw RFC822 is persisted in the blob store whenever fetched (`message.raw_blob_path`, new column) and bulk-fetched from reachable servers at export time.

**Tech Stack:** Rust/Axum, `age` 0.11, `zip` 8.6, sqlx/sea-orm `DbPool`; React/TS, vitest.

**Spec:** `docs/superpowers/specs/2026-09-08-lyra-backup-export-import-design.md` (read first — archive layout, merge keys, error taxonomy).

**Verification baseline before starting:** `cd backend && cargo test --bin lyra_backend` (533 pass, 22 ignored), `cd frontend && npm test` (237 pass), `make lint` clean (7 pre-existing oxlint warnings are baseline).

**Conventions that matter here:**
- Backend tests run with `cargo test --bin lyra_backend` (NOT `--lib`).
- Migrations exist per dialect: `backend/migrations/{sqlite,postgres,mysql}/NNNN_name.{up,down}.sql`; next number is **0024**. SQLite/MySQL use TEXT ids/timestamps; Postgres uses native types.
- Clippy runs with `-D warnings`; format with `make fmt` before every commit.
- Never log credentials or archive bytes; job errors go through `jobs::sanitize_error` / `scrub_error_detail`.

---

### Task 1: Dependencies + `raw_blob_path` migration + entity field

**Files:**
- Modify: `backend/Cargo.toml` (after the `p256` line, end of `[dependencies]`)
- Create: `backend/migrations/sqlite/0024_message_raw_blob.{up,down}.sql`
- Create: `backend/migrations/postgres/0024_message_raw_blob.{up,down}.sql`
- Create: `backend/migrations/mysql/0024_message_raw_blob.{up,down}.sql`
- Modify: `backend/src/entities/message.rs` (after `body_blob_path` field, line ~44)

- [ ] **Step 1: Add dependencies**

```toml
# Backup archives: zip container + age passphrase encryption (rage-compatible).
zip = { version = "8.6", default-features = false, features = ["deflate"] }
age = { version = "0.11", default-features = false, features = ["armor"] }
```

Run `cd backend && cargo check` to update Cargo.lock. If `zip` 8.6's `deflate`
feature name errors, use `features = ["deflate-flate2"]` and note the change.

- [ ] **Step 2: Write the migrations**

`sqlite/0024_message_raw_blob.up.sql` and `mysql/0024_message_raw_blob.up.sql`:

```sql
ALTER TABLE message ADD COLUMN raw_blob_path TEXT;
```

`postgres/0024_message_raw_blob.up.sql`:

```sql
ALTER TABLE message ADD COLUMN raw_blob_path TEXT;
```

All three `.down.sql` files:

```sql
ALTER TABLE message DROP COLUMN raw_blob_path;
```

Check an existing MySQL migration (e.g. `mysql/0023_ai_settings.up.sql`) for
dialect differences first; if MySQL needs `VARCHAR(255)` instead of `TEXT`
for indexed columns — this column is never indexed, so `TEXT` is fine.

- [ ] **Step 3: Entity field**

In `backend/src/entities/message.rs` after `pub body_blob_path: Option<String>,`:

```rust
    /// Raw RFC822 bytes in the blob store, when fetched (view/DKIM/export).
    pub raw_blob_path: Option<String>,
```

- [ ] **Step 4: Verify**

Run: `cd backend && cargo test --bin lyra_backend`
Expected: all 533+ pass (SQLite suite auto-migrates; entity matches schema).

- [ ] **Step 5: Commit**

```bash
git add backend/Cargo.toml backend/Cargo.lock backend/migrations backend/src/entities/message.rs
git commit -m "feat(backup): raw_blob_path column + age/zip deps"
```

---

### Task 2: Persist raw RFC822 whenever fetched

**Files:**
- Modify: `backend/src/sync/store.rs` (new `pub(crate)` setter, next to `update_dkim_verdict`)
- Modify: `backend/src/sync/http.rs` (DKIM path ~line 1097-1139, lazy body-fill path ~line 1226)
- Test: `backend/src/sync/store.rs` test module

- [ ] **Step 1: Write the failing test**

In `store.rs` tests (follow the existing sqlite-fixture pattern used by
neighboring tests — look for a helper building an in-memory `DbPool`):

```rust
#[tokio::test]
async fn set_message_raw_blob_records_path() {
    let db = test_db().await; // use the module's existing fixture name
    // insert account+folder+message via the existing helpers, then:
    set_message_raw_blob(&db, &message_id, "blobs/acc/ab/hash")
        .await
        .unwrap();
    let got = get_message_raw_blob_path(&db, &message_id).await.unwrap();
    assert_eq!(got.as_deref(), Some("blobs/acc/ab/hash"));
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cd backend && cargo test --bin lyra_backend set_message_raw_blob`
Expected: FAIL — function does not exist.

- [ ] **Step 3: Implement the setter/getter**

In `store.rs`, modelled on `update_dkim_verdict` (same entity-built UPDATE,
`id_value` binds, `orm()` execute):

```rust
/// Record where a message's raw RFC822 bytes landed in the blob store.
pub(crate) async fn set_message_raw_blob(
    db: &DbPool,
    message_id: &str,
    rel_path: &str,
) -> Result<(), SyncError> {
    let conn = db.orm();
    let mut u = Sq::update();
    u.table(message::Entity)
        .set(message::Column::RawBlobPath, Expr::val(rel_path))
        .set(message::Column::UpdatedAt, Expr::val(crate::now_text()));
    u.and_where(Expr::col(message::Column::Id).eq(id_value(db, message_id)?));
    conn.execute(&u).await.map_err(orm_err)?;
    Ok(())
}
```

(`Sq`, `Expr`, `message::Entity`, `orm_err`, `id_value`, and the time helper
already exist in `store.rs` — match their actual names/signatures. The getter
`get_message_raw_blob_path` is a one-column SELECT by id, same style.)

- [ ] **Step 4: Store raw at both fetch sites in `sync/http.rs`**

In the DKIM path (`maybe_verify_dkim`, right after `let Some(raw) = raw else { return };`,
~line 1119) and in the lazy body-fill path (where `fetch_bodies`/`download_blob`
raw bytes arrive, ~line 1226), add:

```rust
if let Ok(rel) = crate::blobs::store(&state.data_dir, &row.account_id, &raw).await {
    let _ = crate::sync::store::set_message_raw_blob(db, &row.id, &rel).await;
}
```

(Adjust variable names to each site; failure to persist must never break the
view/DKIM flow — hence `let _`.)

- [ ] **Step 5: Verify + commit**

Run: `cd backend && cargo test --bin lyra_backend` → all pass; `cargo clippy --all-targets --all-features -- -D warnings` clean.

```bash
git add backend/src/sync/store.rs backend/src/sync/http.rs
git commit -m "feat(backup): persist raw RFC822 to blob store when fetched"
```

---

### Task 3: `backup/format.rs` — manifest, mbox writer, sidecar

**Files:**
- Create: `backend/src/backup/mod.rs`
- Create: `backend/src/backup/format.rs`
- Modify: `backend/src/main.rs` (`mod backup;` with the other mod declarations)

- [ ] **Step 1: Failing tests first**

Create `backend/src/backup/format.rs` with the test module written BEFORE the
implementation (run, watch fail, then implement — same file, TDD within one
commit is fine here):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mbox_escapes_from_lines() {
        let raw = b"Subject: x\r\n\r\nFrom nowhere\r\n>From quoted\r\n";
        let mut out = Vec::new();
        write_mbox_message(&mut out, "a@b.com", 1_700_000_000, raw).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.starts_with("From a@b.com 1700000000\r\n"));
        assert!(text.contains("\r\n>From nowhere\r\n"));
        assert!(text.contains("\r\n>>From quoted\r\n"));
        assert!(text.ends_with("\r\n"));
    }

    #[test]
    fn manifest_roundtrip() {
        let m = Manifest {
            format: 1,
            app: "lyra".into(),
            app_version: "0.1.0".into(),
            created_at: "2026-09-08T12:00:00Z".into(),
            sections: Sections {
                settings: true, accounts: 2, messages: 10,
                contacts: 3, calendars: 1, blobs: 4,
            },
        };
        let json = serde_json::to_string(&m).unwrap();
        let back: Manifest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.format, 1);
        assert_eq!(back.sections.messages, 10);
    }

    #[test]
    fn meta_line_roundtrip() {
        let line = MetaLine {
            message_id: Some("<a@b>".into()),
            flags: vec!["seen".into()],
            date: Some("2026-09-08T12:00:00Z".into()),
            sha256: "ab".repeat(32),
            reconstructed: false,
        };
        let s = serde_json::to_string(&line).unwrap();
        let back: MetaLine = serde_json::from_str(&s).unwrap();
        assert_eq!(back.sha256, line.sha256);
    }
}
```

- [ ] **Step 2: Implement**

```rust
//! Archive layout: manifest, mbox escaping, per-folder sidecar lines.
//! Spec: docs/superpowers/specs/2026-09-08-lyra-backup-export-import-design.md §3.

use serde::{Deserialize, Serialize};
use std::io::Write;

pub const FORMAT_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub format: u32,
    pub app: String,
    pub app_version: String,
    pub created_at: String,
    pub sections: Sections,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sections {
    pub settings: bool,
    pub accounts: u32,
    pub messages: u64,
    pub contacts: u32,
    pub calendars: u32,
    pub blobs: u64,
}

/// One `.meta.jsonl` line: everything mbox cannot carry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetaLine {
    pub message_id: Option<String>,
    pub flags: Vec<String>, // "seen" | "flagged"
    pub date: Option<String>,
    pub sha256: String,
    #[serde(default)]
    pub reconstructed: bool,
}

/// Append one message to an mbox stream (RFC 4155): separator line,
/// `>From `-escaped body, trailing blank line.
pub fn write_mbox_message(
    out: &mut impl Write,
    from_addr: &str,
    epoch: i64,
    raw: &[u8],
) -> std::io::Result<()> {
    write!(out, "From {from_addr} {epoch}\r\n")?;
    // Escape any line starting with "From " (and keep existing '>' chains).
    for (i, line) in raw.split(|b| *b == b'\n').enumerate() {
        if i > 0 {
            out.write_all(b"\n")?;
        }
        if line.starts_with(b"From ") || line.starts_with(b">") {
            out.write_all(b">")?;
        }
        out.write_all(line)?;
    }
    out.write_all(b"\r\n")?;
    Ok(())
}

/// Reverse of the escaping in `write_mbox_message`, used by import.
pub fn unescape_mbox_line(line: &[u8]) -> &[u8] {
    if let Some(rest) = line.strip_prefix(b">") {
        rest
    } else {
        line
    }
}
```

Note: the `>`-prefix rule above escapes BOTH `From ` and any `>`-leading line
(this is the common conservative mboxrd choice); import strips exactly one
leading `>` per line. Keep writer and reader consistent — the roundtrip test
is the contract.

Create `backend/src/backup/mod.rs`:

```rust
//! Full-instance backup: age-encrypted zip export/import.
//! Spec: docs/superpowers/specs/2026-09-08-lyra-backup-export-import-design.md

pub mod format;
```

Add `mod backup;` to `main.rs` with the other module declarations.

- [ ] **Step 3: Verify + commit**

Run: `cd backend && cargo test --bin lyra_backend backup` → 3 new tests pass.

```bash
git add backend/src/backup backend/src/main.rs
git commit -m "feat(backup): archive format — manifest, mbox writer, sidecar"
```

---

### Task 4: `backup/crypto.rs` — age passphrase wrapper

**Files:**
- Create: `backend/src/backup/crypto.rs`
- Modify: `backend/src/backup/mod.rs` (add `pub mod crypto;`)

- [ ] **Step 1: Failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let plain = dir.path().join("a.bin");
        let enc = dir.path().join("a.bin.age");
        let dec = dir.path().join("a.out.bin");
        std::fs::write(&plain, b"backup bytes \u{1f600}").unwrap();
        encrypt_file(&plain, &enc, "test-password-1").unwrap();
        decrypt_file(&enc, &dec, "test-password-1").unwrap();
        assert_eq!(std::fs::read(&dec).unwrap(), b"backup bytes \u{1f600}");
    }

    #[test]
    fn wrong_password_is_typed_error() {
        let dir = tempfile::tempdir().unwrap();
        let plain = dir.path().join("a.bin");
        let enc = dir.path().join("a.bin.age");
        let dec = dir.path().join("a.out.bin");
        std::fs::write(&plain, b"x").unwrap();
        encrypt_file(&plain, &enc, "right").unwrap();
        let err = decrypt_file(&enc, &dec, "wrong").unwrap_err();
        assert!(matches!(err, BackupError::InvalidPassword));
    }
}
```

(`tempfile` is already a dev-dependency.)

- [ ] **Step 2: Implement**

```rust
//! age passphrase encryption for the archive file (scrypt recipient;
//! decryptable with the OSS `rage` CLI).

use std::io::{Read, Write};
use std::path::Path;

use crate::backup::BackupError;

pub fn encrypt_file(src: &Path, dst: &Path, password: &str) -> Result<(), BackupError> {
    let plaintext = std::fs::read(src)?;
    let encryptor = age::Encryptor::with_user_passphrase(age::secrecy::SecretString::from(
        password.to_string(),
    ));
    let mut out = age::stream::StreamWriter::new(std::fs::File::create(dst)?)
        // NOTE: exact constructor is `encryptor.wrap_output(writer)`; see below.
        .map_err(|_| BackupError::Crypto("init".into()))?;
    out.write_all(&plaintext)?;
    out.finish().map_err(|_| BackupError::Crypto("finish".into()))?;
    Ok(())
}
```

STOP — write it correctly against age 0.11's actual API:

```rust
pub fn encrypt_file(src: &Path, dst: &Path, password: &str) -> Result<(), BackupError> {
    let plaintext = std::fs::read(src)?;
    let encryptor = age::Encryptor::with_user_passphrase(age::secrecy::SecretString::from(
        password.to_string(),
    ));
    let mut writer = encryptor
        .wrap_output(std::fs::File::create(dst)?)
        .map_err(|e| BackupError::Crypto(e.to_string()))?;
    writer.write_all(&plaintext)?;
    writer
        .finish()
        .map_err(|e| BackupError::Crypto(e.to_string()))?;
    Ok(())
}

pub fn decrypt_file(src: &Path, dst: &Path, password: &str) -> Result<(), BackupError> {
    let file = std::fs::File::open(src)?;
    let decryptor = match age::Decryptor::new(file) {
        Ok(age::Decryptor::Passphrase(d)) => d,
        Ok(_) => return Err(BackupError::Crypto("not a passphrase archive".into())),
        Err(_) => return Err(BackupError::CorruptArchive),
    };
    let mut reader = decryptor
        .decrypt(&age::secrecy::SecretString::from(password.to_string()), None)
        .map_err(|_| BackupError::InvalidPassword)?;
    let mut plaintext = Vec::new();
    reader.read_to_end(&mut plaintext)?;
    std::fs::write(dst, &plaintext)?;
    Ok(())
}
```

Reading the whole zip into memory is acceptable for v1 (archives are
user-scale; note a streaming variant as a comment). If age 0.11's
`SecretString` import path differs, use whatever `age::Encryptor::
with_user_passphrase` expects — pin by compiling, not by guessing.

`BackupError` is defined in Task 9 — for this task, define it temporarily in
`mod.rs` and MOVE it to its final home when Task 9 lands (or define it in
`mod.rs` permanently — your call, one definition only):

```rust
#[derive(Debug, thiserror::Error)]
pub enum BackupError {
    #[error("invalid backup password")]
    InvalidPassword,
    #[error("corrupt archive")]
    CorruptArchive,
    #[error("unsupported backup format")]
    UnsupportedFormat,
    #[error("upload incomplete")]
    UploadIncomplete,
    #[error("crypto: {0}")]
    Crypto(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("db: {0}")]
    Db(#[from] sqlx::Error),
    #[error("zip: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("internal: {0}")]
    Internal(String),
}
```

- [ ] **Step 3: Verify + commit**

Run: `cd backend && cargo test --bin lyra_backend backup` → 2 new tests pass.

```bash
git add backend/src/backup
git commit -m "feat(backup): age passphrase file encryption"
```

---

### Task 5: `backup/export.rs` — snapshot collectors (settings, accounts, contacts, calendars)

**Files:**
- Create: `backend/src/backup/export.rs`
- Modify: `backend/src/backup/mod.rs` (add `pub mod export;`)

Collectors write into a staging dir `data/backups/staging/<uuid>/`. All async
DB reads happen here (zip+age happen later, blocking). Exact content:

- [ ] **Step 1: `settings.json`**

Fetch `lyra_user.ui_state` (see `auth/db.rs` `update_ui_state`'s sibling read
in `auth/handlers.rs::auth_me`) for the user and write:

```json
{ "ui_state": <parsed ui_state JSON or null> }
```

- [ ] **Step 2: `accounts/<n>.json`**

For each `mail_account` row of the user (ordered by `created_at`), decrypt
`credential` (and `smtp_credential`, `pim_credential` when present):
`AuthState::get_user_dek(db, user_id)` → `crypto::decrypt(dek, &EncryptedCredential)`
— the credential columns hold JSON-serialized `EncryptedCredential`
(`crypto.rs:27`). Write per account:

```json
{
  "index": 0,
  "display_name": "...", "email_address": "...", "protocol": "imap",
  "auth_type": "password", "imap_host": "...", "imap_port": 993,
  "imap_security": "tls", "jmap_base_url": null, "smtp_host": "...",
  "smtp_port": 465, "smtp_security": "tls", "smtp_auth_type": null,
  "signature": null, "carddav_url": null, "caldav_url": null,
  "sync_enabled": true, "receive_protocol": "imap", "send_protocol": "smtp",
  "credential": <decrypted inner JSON value>,
  "smtp_credential": <decrypted inner JSON value or null>,
  "pim_credential": <decrypted inner JSON value or null>
}
```

Include the folder list inline: `"folders": [{"id","external_id","name",
"parent_external_id","role","role_override","sort_order"}]` — parent linkage
by `external_id` (IMAP wire name / JMAP id), since UUIDs are per-instance.

- [ ] **Step 3: `contacts.vcf`**

Concatenate every `contact.vcard_blob` (skip NULL/empty) separated by CRLF.
If `vcard_blob` is itself a blob-store path in some rows (check the CardDAV
persist code before writing this), resolve via `blobs::read`.

- [ ] **Step 4: `calendars/<n>.ics`**

Group `calendar_event` rows by `calendar_id`; write one file per `calendar`
row (`<n>` = calendar row index in created_at order) containing
`BEGIN:VCALENDAR` + each event's `icalendar_blob` (skip NULL) + `END:VCALENDAR`.
Record `{index, name, color, description, timezone}` per calendar in the
account JSON sidecar `calendars.json` at archive root.

- [ ] **Step 5: Attachment/avatar blobs**

Walk `attachment` rows joined to messages of this user; copy each
`storage_path` blob to staging `blobs/<sha256-from-path>` (basename of the
relative path — it IS the sha256, see `blobs::relative_blob_path`). Same for
`contact.photo_path`. Deduplicate by basename.

- [ ] **Step 6: Unit tests**

Fixture: in-memory SQLite `DbPool` (existing test pattern in `store.rs` /
`auth/tests.rs` — `install_test_master_key()` first) with one account, one
folder, one contact, one event; run collectors into a `tempfile::tempdir`;
assert file existence + parse the account JSON and check `credential` is the
DECRYPTED inner value (not the `EncryptedCredential` envelope).

- [ ] **Step 7: Verify + commit**

`cargo test --bin lyra_backend backup::export` green; clippy clean; commit
`feat(backup): export collectors — settings, accounts, contacts, calendars, blobs`.

---

### Task 6: `export.rs` — mail collection with raw resolution

**Files:**
- Modify: `backend/src/backup/export.rs`

- [ ] **Step 1: Per-folder mbox + sidecar**

For each account (index `n`) and each folder: query messages
(`id, external_id, message_id_header, subject, from_address, flags, date,
raw_blob_path, body_text, body_html`) ordered by `date`. For each message,
resolve raw bytes in this order:

1. `raw_blob_path` → `blobs::read`.
2. Server fetch: batch per folder — IMAP: parse `external_id` (`folder:uid`,
   `parse_imap_uid` in `sync/http.rs`) and call the existing
   `fetch_bodies(&[uids])` after `connect_imap_for_account`; JMAP: `blob_id`
   from `Email/get` then `download_blob`. Cap batches at 50 UIDs; on success
   also `set_message_raw_blob` (Task 2's function) so the next export is local.
3. Reconstruct: build a minimal RFC822 message
   (`From/To/Cc/Subject/Date/Message-ID` headers from columns + MIME html/text
   parts) and mark `reconstructed: true` in the sidecar line. Server
   unreachable → skip straight here.

Write via `write_mbox_message` (epoch = `date` parsed, fallback
`received_at`); append the `MetaLine` JSON + `\n` to `<folder-id>.meta.jsonl`;
`sha256` = `blobs::sha256_hex(&raw)`.

- [ ] **Step 2: folders.json**

Per account write `mail/<n>/folders.json`: `{ "<folder-uuid>": {"path":
external_id_or_name, "role": ...} }` — import uses this to recreate/match
folders (UUIDs are archive-local; matching is by path+account).

- [ ] **Step 3: Progress**

After each folder: `kv.set(&format!("backup:progress:{job_id}"),
&json!({"phase":"mail","account":n,"folder":path,"messages_done":x}).to_string(), Some(3600))`.

- [ ] **Step 4: Tests**

Fixture DB with two messages (one with `raw_blob_path` pointing at a real
blob written into a temp `data_dir`, one with only parsed bodies →
reconstructed). No server available in tests → resolution order exercises
paths 1 and 3. Assert: mbox contains both, sidecar flags the reconstructed
one, sha256 matches.

- [ ] **Step 5: Verify + commit**

`cargo test --bin lyra_backend backup` green; commit
`feat(backup): mail export with raw-resolution chain`.

---

### Task 7: Assembly — zip → age → artifact registry

**Files:**
- Modify: `backend/src/backup/export.rs` (`run` entry)
- Create: `backend/src/backup/artifacts.rs`

- [ ] **Step 1: `pub async fn run(state, user_id, job_id, artifact_id, password)`**

Sequence: staging dir → collectors (Task 5) → mail (Task 6) → manifest with
counts → `spawn_blocking`: zip staging dir into `staging/<job>.zip`
(`zip::ZipWriter`, `SimpleFileOptions::default().compression_method(
zip::CompressionMethod::Deflated)`, walk dir recursively, store relative
paths) → `crypto::encrypt_file(zip, data/backups/<artifact_id>.lyra,
password)` → remove staging dir + zip → write kv report
(`backup:report:<job_id>`, `{"ok":true,"artifact_id":...,"sections":{...},
"reconstructed":N}`, no TTL) and registry append.

- [ ] **Step 2: `artifacts.rs`**

```rust
//! Artifact registry in kv: `backup:artifacts:{user_id}` = JSON array of
//! ArtifactMeta { id, filename, size_bytes, created_at }.

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ArtifactMeta {
    pub id: String,
    pub filename: String, // lyra-backup-YYYYMMDD-HHmmss.lyra
    pub size_bytes: u64,
    pub created_at: String,
}

pub async fn list(kv: &Arc<dyn KvStore>, user_id: &str) -> Result<Vec<ArtifactMeta>, BackupError>;
pub async fn add(kv: &Arc<dyn KvStore>, user_id: &str, meta: ArtifactMeta) -> Result<(), BackupError>;
pub async fn remove(kv: &Arc<dyn KvStore>, user_id: &str, id: &str) -> Result<Option<ArtifactMeta>, BackupError>;
pub fn artifact_path(data_dir: &Path, id: &str) -> PathBuf; // data_dir/backups/<id>.lyra — id is a uuid, never user input reaching the filesystem unchecked: validate id parses as Uuid before joining
```

Tests with `MemoryKv`: add/list/remove roundtrip; path traversal rejected
(`id = "../etc"` → error).

- [ ] **Step 3: Verify + commit**

`cargo test --bin lyra_backend backup` green; commit
`feat(backup): archive assembly + artifact registry`.

---

### Task 8: Job payloads + worker arms

**Files:**
- Modify: `backend/src/jobs.rs` (JobPayload enum line 27-41, `payload_kind` line 138, `process_job` match line 457)
- Modify: `backend/src/backup/export.rs` / `import.rs` (entry points matching the arms)

- [ ] **Step 1: New payload variants**

```rust
    ExportBackup {
        user_id: String,
        job_id: String,      // == jobs row id; filled by caller for progress kv
        artifact_id: String,
        password_wrapped: String, // EncryptedCredential JSON under the user DEK
    },
    ImportBackup {
        user_id: String,
        job_id: String,
        upload_id: String,
        password_wrapped: String,
    },
```

`payload_kind` gains `"export_backup"` / `"import_backup"`.

Why wrapped: the jobs table must never hold the archive password in
plaintext. Enqueue sites encrypt with `crypto::encrypt(&dek, password.as_bytes())`
+ serialize; worker decrypts via `AuthState::get_user_dek`.

- [ ] **Step 2: `process_job` arms**

```rust
        JobPayload::ExportBackup { user_id, job_id, artifact_id, password_wrapped } => {
            let r = backup::export::run(&app_state, &user_id, &job_id, &artifact_id, &password_wrapped).await;
            finalize_backup_job(db, &job.id, r).await?;
        }
        JobPayload::ImportBackup { user_id, job_id, upload_id, password_wrapped } => {
            let r = backup::import::run(&app_state, &user_id, &job_id, &upload_id, &password_wrapped).await;
            finalize_backup_job(db, &job.id, r).await?;
        }
```

`finalize_backup_job` (in jobs.rs, private): Ok → `mark_completed`; Err →
write kv report `{"ok":false,"error":code}` then the existing failure path
(`sanitize_error`/`scrub_error_detail` — never the raw error if it could
carry secrets; map `BackupError` to its `#[error]` string only).

Note `process_job` receives `app: &App` — check how existing arms reach
`AuthState`/`data_dir`; if only `App` is available, add what's needed to the
arm's reachable state (look at how `SendMessage` reaches account creds and
mirror that).

- [ ] **Step 3: Tests**

`payload_kind` mapping test (serde tag roundtrip: `"export_backup"`). The
full run path is covered by the Task 13 roundtrip.

- [ ] **Step 4: Verify + commit**

`cargo test --bin lyra_backend jobs` green; commit
`feat(backup): export/import job payloads on the worker pool`.

---

### Task 9: `backup/http.rs` — export/artifact endpoints

**Files:**
- Create: `backend/src/backup/http.rs`
- Modify: `backend/src/backup/mod.rs`, `backend/src/main.rs` (`.merge(backup::routes())` next to `.merge(push::routes())`)

- [ ] **Step 1: Routes** (pattern: `push/http.rs`)

```rust
pub fn routes() -> Router<AuthState> {
    Router::new()
        .route("/api/v1/backup/export", post(start_export))
        .route("/api/v1/backup/jobs/{job_id}", get(job_status))
        .route("/api/v1/backup/artifacts", get(list_artifacts))
        .route("/api/v1/backup/artifacts/{id}/download", get(download_artifact))
        .route("/api/v1/backup/artifacts/{id}", delete(delete_artifact))
}
```

- `start_export`: `Json<ExportRequest{password}>`; enforce `password.len() >= 8`
  → else 400. Mint `artifact_id` (Uuid v7), enqueue `ExportBackup` (payload's
  `job_id` = the id returned by `jobs::enqueue` — enqueue first with a
  placeholder then update, or generate the uuid yourself and pass a
  `jobs::enqueue_with_id` — check which is cleaner; adding an id parameter
  to `enqueue` is acceptable), respond `202 {"job_id","artifact_id"}`.
- `job_status`: read jobs row status by id (scoped to user via payload
  user_id) + merge kv progress/report → `{status, progress?, report?}`.
- `list_artifacts` → `artifacts::list`.
- `download_artifact`: `artifacts::path` + `tower_http::services::ServeFile`
  or manual `tokio::fs::read` + headers (`Content-Type: application/octet-stream`,
  `Content-Disposition: attachment; filename="<meta.filename>"`). Check the
  id is in the user's registry before serving.
- `delete_artifact`: remove file + registry entry.

- [ ] **Step 2: `BackupError` IntoResponse**

Map: InvalidPassword → 422 `invalid_backup_password`; UnsupportedFormat →
422 `unsupported_backup_format`; CorruptArchive → 422 `corrupt_archive`;
UploadIncomplete → 400 `upload_incomplete`; Io/Zip/Json/Internal → 500.
Body via `ApiErrorBody::new` (see `sync/types.rs:75-130`).

- [ ] **Step 3: Tests**

Handler-level with `tower::ServiceExt::oneshot` (pattern from existing http
tests if any, else store-level only): wrong-id download → 404; short
password → 400.

- [ ] **Step 4: Verify + commit**

Full suite green; commit `feat(backup): export + artifact HTTP endpoints`.

---

### Task 10: Chunked import upload endpoints

**Files:**
- Modify: `backend/src/backup/http.rs` (routes), `backend/src/backup/import.rs` (new)

- [ ] **Step 1: Routes**

```rust
        .route("/api/v1/backup/import/uploads", post(start_upload))
        .route("/api/v1/backup/import/uploads/{id}/chunks/{n}", put(put_chunk))
        .route("/api/v1/backup/import/uploads/{id}/finish", post(finish_upload))
```

- `start_upload` → `{upload_id}`; creates `data/backups/staging/upload-<id>.part`.
  Registry kv `backup:uploads:{user_id}`: `{id, size, received, created_at}`.
- `put_chunk`: raw body (`Bytes`), `n` = chunk index; write at
  `n * CHUNK_SIZE` (8 MiB) via `tokio::fs` seek+write; track `received` in kv;
  reject total > 4 GiB.
- `finish_upload`: `Json<{password, total_chunks}>`; verify all chunks
  present → rename to `upload-<id>.lyra`; enqueue `ImportBackup`; 202 `{job_id}`.
  Missing chunks → `UploadIncomplete`.

- [ ] **Step 2: Tests**

Store-level: chunk write/verify logic against a tempdir (split a 20 MiB
buffer into 3 chunks, finish, sha256 matches); missing chunk → typed error.
`MemoryKv` for the registry.

- [ ] **Step 3: Verify + commit**

Commit `feat(backup): chunked import upload`.

---

### Task 11: `import.rs` — decrypt + validate + open

**Files:**
- Modify: `backend/src/backup/import.rs`

- [ ] **Step 1: `run` prelude**

`crypto::decrypt_file` (InvalidPassword surfaces through the job report) →
`zip::ZipArchive` (spawn_blocking) → read `manifest.json` → `app == "lyra"`
and `format == FORMAT_VERSION` else `UnsupportedFormat` → return an
`Archive` struct holding the extracted staging dir + manifest. Extract to a
fresh staging subdir (`zip::ZipArchive::extract`-style manual loop: for each
entry, sanitize name — reject absolute paths and `..` — then write).

- [ ] **Step 2: Tests**

Build a tiny archive in-test via the Task 7 writer (shared test helper:
`build_test_archive(dir)`) → open/validate OK; tamper manifest `format: 99` →
UnsupportedFormat; entry named `../evil` → rejected.

- [ ] **Step 3: Commit** `feat(backup): archive open + manifest validation`.

---

### Task 12: Import merge — settings, accounts, folders

**Files:**
- Modify: `backend/src/backup/import.rs`

- [ ] **Step 1: settings**

`settings.json` present → `lyra_user.ui_state = serialized ui_state` via the
same update path as `auth/handlers.rs::update_ui_state` (single-user
singleton; wholesale apply per spec).

- [ ] **Step 2: accounts**

For each `accounts/<n>.json`: match existing on
`(protocol, email_address)` (case-insensitive email) — host matching is
unreliable across auto-config variants; spec's "(protocol, host, username)"
resolves in practice to protocol+email. Match → record `id_map[n] =
existing_id`, credentials untouched. No match → insert a new `mail_account`
row (new Uuid v7; `credential`/`smtp_credential`/`pim_credential` re-encrypted
with THIS instance's user DEK via `crypto::encrypt`; `is_active=true`,
`sync_enabled` from archive; `last_sync_at=NULL`). Record `id_map`.

- [ ] **Step 3: folders**

For each mapped account, walk `mail/<n>/folders.json` + the account JSON's
`folders` array: match by `(account_id, external_id)` (fallback: name+parent
chain); missing → insert folder row (new id, `external_id`, name, parent
resolved through the same map, role/role_override/sort_order). Build
`folder_map[n][archive_folder_uuid] = local_uuid`.

- [ ] **Step 4: Tests**

Fixture: pre-existing matching account → import does not create a duplicate
and does not touch its credential column; unknown account → inserted with
decryptable credentials (roundtrip through `get_user_dek_and_credential`).
Folders: nested parent/child recreated.

- [ ] **Step 5: Commit** `feat(backup): import merge — settings/accounts/folders`.

---

### Task 13: Import merge — messages, blobs, contacts, calendars + report

**Files:**
- Modify: `backend/src/backup/import.rs`

- [ ] **Step 1: messages**

Per account `n`, per `<folder-id>.mbox`: split the mbox into messages
(separator `^From .* \d+$` lines; strip ONE leading `>` per body line —
the inverse of the Task 3 writer), align with `.meta.jsonl` lines by index
(counts must match, else `CorruptArchive` for that folder → recorded,
continue). Per message: dedupe key = `message_id_header` when present else
`sha256`; skip if a message with same `(account_id, external_id =
"import:<sha256>")` OR same `(account_id, message_id_header)` exists.
Insert via the existing `message_insert` builder (new folder_id from
`folder_map`, flags from meta, `external_id = format!("import:{sha256}")`,
bodies parsed from raw via the lenient mail-parser path used by
`parse_header_metadata`/`extract_mime_parts`, `raw_blob_path` = freshly
stored blob). Also copy referenced attachment blobs from `blobs/<sha256>`
into the local blob store (`blobs::store` dedupes) — only for messages
actually inserted.

- [ ] **Step 2: contacts/calendars**

Match by UID inside the vCard/iCal blob text (`UID:` line) per account;
existing → skip; new → insert row with blob stored.

- [ ] **Step 3: report**

`backup:report:<job_id>` = `{ok:true, report:{settings:true,
accounts:{inserted,skipped}, folders:{...}, messages:{inserted,skipped,
failed}, contacts:{...}, calendars:{...}, errors:[first 10 strings]}}`.
Staging cleanup (upload + extracted dir) in all outcomes.

- [ ] **Step 4: The roundtrip integration test**

`backup/tests`-style (in-module) test: fixture DB A (account, folders, 3
messages — one raw-backed, one reconstructed, one with attachment; contact;
event; ui_state) → `export::run` to a real `.lyra` in tempdir → fresh DB B
→ `import::run` → assert counts, flags, credential decryptability, ui_state.
Then import AGAIN into B → report all-skipped, counts unchanged
(idempotency).

- [ ] **Step 5: postgres_live seam**

Add the new query helpers to the existing postgres_live roundtrip pattern
(`--ignored`, `LYRA_TEST_DATABASE_URL`) per AGENTS.md, and mysql_live where
the seam exists.

- [ ] **Step 6: Commit** `feat(backup): import merge — messages/blobs/PIM + report`.

---

### Task 14: Frontend `lib/backup.ts`

**Files:**
- Create: `frontend/src/lib/backup.ts`
- Test: `frontend/src/lib/backup.test.ts`

Model on `lib/push.ts` (module doc, `api()` wrappers, colocated tests).
Exports: `BackupArtifact`, `BackupJobStatus`, `ImportReport` types;
`startExport(password)`, `backupJobStatus(id)`, `listArtifacts()`,
`deleteArtifact(id)`, `downloadArtifact(id, filename)` (apiBlob +
createObjectURL pattern from `opengpg-api.ts:173-181`), `uploadBackup(file,
password, onProgress)` (slice into 8 MiB chunks, sequential PUT, then
finish), `MIN_PASSWORD = 8` + `validatePassword(pw)` shared by UI and tests.

Tests (vitest, mock `api`/fetch like existing lib tests): chunk math (20 MiB
file → 3 chunks, correct sizes), validatePassword rules, report type guard.

Commit `feat(backup): frontend backup API client`.

---

### Task 15: Settings Backup section

**Files:**
- Create: `frontend/src/components/backup-settings.tsx`
- Modify: `frontend/src/components/settings-page.tsx` (union type line ~95,
  `navItems` ~853, `sectionMeta` ~905, body block — mirror how
  `AiSettingsCard` is wired at line ~1513)
- Modify: `frontend/src/i18n/en.json`, `frontend/src/i18n/zh.json` (`settings.backup.*` keys)

Component (self-contained card like `notification-settings.tsx`):
- **Export card**: password + confirm inputs (mismatch/short → inline error,
  Create disabled — same rule as the captcha settings: any empty field
  disables the button), Create → poll `backupJobStatus` every 2s → download
  button on completion.
- **Archives card**: list with size/date, download + delete buttons.
- **Import card**: file picker (accept `.lyra`) → password → chunked upload
  with % progress → poll job → render the per-section report
  (inserted/skipped/failed per section, first errors listed).
- en + zh strings for all of the above.

Tests: existing settings component tests pattern if present; at minimum
lib-level coverage already landed in Task 14.

Commit `feat(backup): Settings Backup section (en/zh)`.

---

### Task 16: OpenAPI, docs, final gates

**Files:**
- Modify: `docs/openapi/api-v1.yaml` (new `backup` tag + the 8 endpoints,
  request/response schemas, error codes from Task 9)
- Modify: `README.md` features list (one line), `docs/specs/2026-08-26-lyra-http-api-surface.md` (endpoint table row)

- [ ] **Step 1: OpenAPI + docs** — match existing YAML style exactly.

- [ ] **Step 2: Full gates**

```bash
make fmt && make lint && make test
```

Expected: 0 errors; backend 533+16-ish new tests pass; frontend 237+ new
tests pass; gitleaks clean. No new oxlint warnings beyond the 7 baseline.

- [ ] **Step 3: Live smoke (local OrbStack)**

Rebuild local container with proxy args, create a backup from the UI,
download, `rage -d` it externally to prove interop, import into a scratch
SQLite `DATABASE_URL` instance, confirm mail renders.

- [ ] **Step 4: Commit** `docs(backup): OpenAPI + README`.

---

## Self-review notes (completed by the plan author)

- Spec §3 archive layout ↔ Tasks 3/5/6/7 (writer) and 11–13 (reader). ✓
- Spec §4 raw chain ↔ Tasks 1/2/6. ✓  Spec §5/§6 job flow ↔ Tasks 7/8/9/10. ✓
- Spec §7 API ↔ Tasks 9/10/16. ✓  Spec §8 UI ↔ Tasks 14/15. ✓
- Spec §9 errors ↔ Tasks 4/9. ✓  Spec §10 tests ↔ every task + Task 13
  roundtrip + Task 16 live smoke. ✓
- Type/name consistency: `BackupError` (Task 4, used 9–13), `MetaLine`
  (3 ↔ 13), `ArtifactMeta` (7 ↔ 9), `write_mbox_message`/`unescape_mbox_line`
  (3 ↔ 13), `set_message_raw_blob` (2 ↔ 6), `id_map`/`folder_map`
  (12 ↔ 13), job payload fields (8 ↔ 9/10). ✓
