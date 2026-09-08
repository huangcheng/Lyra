# Lyra — Full-Instance Backup: Export / Import

Date: 2026-09-08
Status: approved (brainstorm decisions locked by user)

## 1. Goals

- One-command full backup of a Lyra instance into a single portable file:
  mail (all accounts/folders), account configs **with credentials**, app
  settings, contacts, calendars.
- Restore onto a fresh or live instance with **additive merge** semantics:
  import never deletes or overwrites existing mail; running it twice is a
  no-op.
- The archive is **password-encrypted** end to end and remains decryptable
  outside Lyra (standard tooling).

## 2. Non-goals (v1)

- No server upload of imported mail (no IMAP APPEND / JMAP import). Imported
  messages live in Lyra's DB only. That is a separate, later feature.
- No wipe-and-restore mode.
- No scheduled/automatic backups (the jobs infra makes this easy later).
- No multi-user semantics; the file carries the single user's data.
- Push subscriptions (Redis kv, device-bound) and sessions are not exported.
- OpenPGP private keys are **not** exported in v1 (separate threat review).

## 3. Archive format

File: `lyra-backup-<YYYYMMDD-HHmmss>.lyra` = **age passphrase encryption**
(Rust `age` crate, scrypt recipient) over a **zip**. Interoperable with the
`rage` CLI: `rage -d backup.lyra > backup.zip`.

Zip layout:

```
manifest.json
settings.json                 # ui_state, notification prefs/mutes, favorites,
                              # folder role overrides, locale/theme
accounts/<n>.json             # account config incl. decrypted credentials;
                              # <n> is a 0-based index shared with mail/<n>/
mail/<n>/folders.json         # folder id → {path, role} for this account
mail/<n>/<folder-id>.mbox     # RFC 4155, raw RFC822 bytes
mail/<n>/<folder-id>.meta.jsonl # one line per message:
                              # {message_id, flags:[seen,flagged], date, sha256}
contacts.vcf                  # all contacts, vCard 4
calendars/<n>.ics              # one per calendar (0-based index); names live in
calendars.json                # [{index, name, color, description, timezone}]
blobs/<sha256>                # attachment + avatar blobs referenced above
```

Zip entry names never embed user-controlled path segments: folders are
keyed by their UUID (IMAP folder paths can contain `/`), and `<n>` indexes
into `accounts/`.

`manifest.json`:

```json
{
  "format": 1,
  "app": "lyra",
  "app_version": "<semver>",
  "created_at": "2026-09-08T12:00:00Z",
  "sections": { "settings": true, "accounts": 3, "messages": 12340,
                "contacts": 210, "calendars": 2, "blobs": 4800 }
}
```

Format version gates import: unknown major `format` → hard error.

### mbox + sidecar

- mbox carries the raw RFC822 bytes with standard `>From ` escaping and a
  `From <addr> <epoch>` separator line. Bytes are the source of truth.
- The `.meta.jsonl` sidecar carries what mbox can't: read/starred flags,
  internal date, Message-ID, and the SHA-256 of the raw bytes (import
  verifies integrity per message; mismatch → skip + report, never abort).

## 4. Raw RFC822 storage

**Today Lyra does not keep raw messages** (parsed `body_html`/`body_text` +
attachment blobs only; IMAP sync fetches `HEADER.FIELDS` metadata only,
JMAP sync fetches parsed body values — raw bytes exist only transiently at
view/DKIM time). Backup therefore works like this:

- A new `message.raw_blob_path` column (SQLite + PostgreSQL + MySQL
  migrations) records a content-addressed blob (`blobs::store`) holding the
  raw RFC822 bytes.
- Raw is persisted **whenever it is already fetched**: the lazy body-fill
  path and the DKIM path store the bytes they downloaded instead of
  dropping them. The sync loop's fetch profile is unchanged (no bulk body
  fetch during sync).
- Export resolution order per message: **stored raw blob** → **fetch from
  the source server at export time** (batch per folder for reachable
  accounts; also fills `raw_blob_path` for next time) → **reconstruct**
  from parsed parts (marked `reconstructed: true` in the sidecar; the export
  report carries warnings but no per-message reconstructed count). A
  dead/decommissioned account still exports its parsed content instead of
  vanishing silently.

## 5. Export flow

`POST /api/v1/backup/export {password}` enqueues a `backup_export` job
(existing `jobs.rs` worker pool; new `JobPayload` variant). The password
travels in the job payload **encrypted with the user's DEK** (the jobs
table never holds it in plaintext):

1. Snapshot reads per table (accounts, folders, messages, attachments,
   contacts, calendars, user ui_state).
2. Decrypt account credentials in memory only (existing user-DEK helpers);
   write them into `accounts/<n>.json` — protected by the archive-level
   age layer, never logged.
3. Stream zip into `data/backups/staging/<job>.zip`, then age-encrypt to
   `data/backups/<id>.lyra`, then delete the staging zip.
4. Progress and the final report live in kv (`backup:progress:<job_id>` /
   `backup:report:<job_id>`) — the jobs table has no result columns; the
   artifact registry is kv `backup:artifacts:<user_id>` (JSON list of
   `{id, filename, size, created_at}`).

`GET /api/v1/backup/jobs/<id>` → status/progress.
`GET /api/v1/backup/artifacts` → list; `GET …/artifacts/<id>/download` →
authenticated file download. `DELETE` removes an artifact.

## 6. Import flow

1. Chunked upload: `POST /api/v1/backup/import/uploads` → upload id;
   `PUT …/uploads/<id>/chunks/<n>` (raw bytes); `POST …/uploads/<id>/finish
   {password}` enqueues a `backup_import` job. Chunking sidesteps the
   Cloudflare 100 MB body cap on production.
2. Job: age-decrypt to staging zip (wrong password → typed
   `invalid_backup_password`), open zip, validate manifest + format
   version (`unsupported_backup_format`).
3. Additive merge, per section. Merges are deliberately **not**
   transaction-wrapped: every operation is idempotent, partial failures are
   collected per item, and re-running the import heals incomplete sections
   (messages additionally self-heal rows whose raw blob/attachments are
   missing):
   - **settings**: applied wholesale (single-user singleton blob).
   - **accounts**: match on `(protocol, email_address)` case-insensitive
     (host matching is unreliable across auto-config variants); matched →
     keep existing id (credentials left untouched); unmatched → insert with
     new id, credentials re-encrypted under this instance's master key.
   - **folders**: match by account + full path; map to existing/new ids.
   - **messages**: match by `message_id_header` within the account
     (fallback: raw-bytes SHA-256); existing → skip; new → insert from the
     mbox bytes (re-parse headers via the lenient mail-parser path), set
     flags/date from the sidecar, store raw blob, link imported attachment
     blobs. `external_id` = `import:<sha256>` so sync never confuses them
     with server messages.
   - **contacts/events**: match by UID; existing → skip.
   - **blobs**: content-addressed — existing hashes are skipped for free.
4. Per-item failures (bad mbox line, checksum mismatch, one corrupt vCard)
   are collected, never fatal. Job ends with a report:
   `{inserted, skipped, failed}` per section plus the first N error strings.
5. Staging files are deleted on completion or failure; staging dir is
   `data/backups/staging/` with 0700 perms, artifact files 0600.

## 7. API surface (`/api/v1`, client-agnostic — OpenAPI updated)

| Endpoint | Purpose |
|---|---|
| `POST /backup/export` | enqueue export, returns job id |
| `GET /backup/jobs/{id}` | job status/progress/result report |
| `GET /backup/artifacts` | list completed archives |
| `GET /backup/artifacts/{id}/download` | download `.lyra` |
| `DELETE /backup/artifacts/{id}` | delete archive |
| `POST /backup/import/uploads` | start chunked upload |
| `PUT /backup/import/uploads/{id}/chunks/{n}` | upload chunk |
| `POST /backup/import/uploads/{id}/finish` | enqueue import job |

## 8. Frontend

Settings gains a **Backup** section (en/zh):

- Export: password + confirm fields (min length 8), "Create backup" →
  progress → download button. Artifacts list with download/delete.
- Import: file picker → chunked upload with progress bar → password field →
  "Import" → progress → the merge report rendered per section.

All logic in `src/lib/backup.ts` with colocated vitest coverage (chunking,
report rendering); the page component stays thin.

## 9. Error handling

Typed `SyncError`-style errors: `invalid_backup_password`,
`unsupported_backup_format`, `corrupt_archive`, `upload_incomplete`.
No credential or archive bytes in logs; job errors pass through
`scrub_error_detail`.

## 10. Testing

- Roundtrip: export a fixture DB (SQLite) → import into a fresh DB → assert
  account/folder/message/contact/calendar counts, flags, and re-login-able
  credentials survive.
- Idempotency: import the same archive twice → second report is all-skipped.
- Wrong password and truncated-archive error paths.
- mbox `>From ` escaping roundtrip; sidecar checksum mismatch → skip+report.
- New SQL seams get `postgres_live` (and `mysql_live` where mirrored)
  roundtrip tests per AGENTS.md.
- Merge-key unit tests for each section.

## 11. Security notes

- age scrypt passphrase recipient; minimum password length enforced at both
  API and UI.
- Credentials exist decrypted only in memory during export/import.
- Artifact and staging files are mode 0600/0700 under `data/backups/`.
- The OpenAPI spec documents that `accounts/*.json` inside the archive
  contains plaintext credentials so users understand the sensitivity.
