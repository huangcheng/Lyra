# Lyra — Data Model Spec (Dual-DB)

> **Data access:** all queries go through SeaORM 2.0 entities (`backend/src/entities/`) over one runtime pool selected by `DATABASE_URL` (SQLite or PostgreSQL; both compile in). Ids are app-generated UUIDv7 stored as TEXT on SQLite and native UUID on PostgreSQL; dialect differences bind in one seam. FTS (migration 0009) stays engine-specific raw SQL by design.


**Date:** 2026-08-20  
**Status:** Draft  
**Companion:** Product spec (`docs/product/2026-08-20-lyra-v1-product-spec.md`), Engineering standards (`docs/specs/2026-08-20-lyra-engineering-standards.md`)

---

## 1. Dual-DB strategy

Lyra must run on **SQLite** (simplest single-box) and **PostgreSQL** (operators who prefer it). The schema is written once in migration files that target both engines.

### 1.1 Principles

- **One migration set.** Migrations are plain SQL written in the subset that both SQLite and PostgreSQL accept (avoiding `SERIAL`, `JSONB`-only operators, Postgres-specific index types in the migration itself).
- **ORM / query layer:** Use an abstraction (e.g. `sqlx` with feature flags, or a thin repository trait) so queries compile against both. Column types use the most portable form: `INTEGER` / `BIGINT` for IDs, `TEXT` for UUIDs, `TIMESTAMP` stored as ISO-8601 text or Unix epoch integer.
- **IDs:** Application-generated UUIDv7 (sortable, no DB-specific sequence). Stored as `TEXT` (SQLite) or `UUID` (Postgres) — the query layer normalises.
- **Timestamps:** Stored as ISO-8601 `TEXT` in SQLite and `TIMESTAMPTZ` in Postgres. The repository layer serialises/deserialises consistently.
- **JSON columns:** Use `TEXT` in SQLite (JSON1 extension for queries) and `JSONB` in Postgres. Migrations avoid JSON-path operators that only Postgres supports; application code handles JSON parsing.

### 1.2 Migration approach

1. Migrations live in `backend/migrations/` as numbered SQL files: `0001_init.up.sql`, `0002_add_threads.up.sql`, etc.
2. Each file contains statements that are valid on **both** engines. Where a type differs, the migration runner applies the correct DDL based on the active backend (feature flag or runtime detection).
3. A `schema_migrations` table tracks applied versions.
4. Backward-compatible only: columns are added (nullable or with default), never removed or renamed in the same version. A later migration can backfill and then drop.
5. Down-migrations are provided for development convenience but are not run in production.

---

## 2. Tables

All tables include the following implicit columns unless noted:

| Column | Type | Notes |
|--------|------|-------|
| `id` | `TEXT` / `UUID` | Application-generated UUIDv7, primary key |
| `created_at` | `TIMESTAMPTZ` / `TEXT` | Row creation time |
| `updated_at` | `TIMESTAMPTZ` / `TEXT` | Last modification time |

### 2.1 `lyra_user`

Single-user v1. Schema shaped so a `user_id` FK can be added to other tables later for multi-user.

| Column | Type | Notes |
|--------|------|-------|
| `id` | PK | Single row for v1 |
| `username` | `TEXT NOT NULL UNIQUE` | Login identifier |
| `password_hash` | `TEXT NOT NULL` | Argon2id hash |
| `totp_secret` | `TEXT` | Encrypted TOTP secret (AES-256-GCM under the user DEK); NULL if 2FA disabled |
| `totp_enabled` | `BOOLEAN NOT NULL DEFAULT FALSE` | |
| `encrypted_dek` | `TEXT` | Per-user DEK, wrapped with the KEK derived from `LYRA_MASTER_KEY` (see §3) |
| `display_name` | `TEXT` | |
| `locale` | `TEXT NOT NULL DEFAULT 'en'` | Preferred UI language |

### 2.2 `mail_account`

One row per connected mail account (e.g. alice@work.com, alice@personal.example).

| Column | Type | Notes |
|--------|------|-------|
| `id` | PK | |
| `user_id` | `TEXT NOT NULL` | FK → `lyra_user.id`; single-user v1 but FK present |
| `display_name` | `TEXT` | Human label ("Work", "Personal") |
| `email_address` | `TEXT NOT NULL` | Primary address |
| `protocol` | `TEXT NOT NULL` | `jmap` or `imap` |
| `auth_type` | `TEXT NOT NULL` | `password`, `oauth2`, `app_password` |
| `credential` | `TEXT NOT NULL` | **Encrypted** JSON blob (see §3) |
| `imap_host` | `TEXT` | IMAP server hostname (null if JMAP) |
| `imap_port` | `INTEGER` | |
| `imap_security` | `TEXT` | `tls`, `starttls`, `none` |
| `jmap_base_url` | `TEXT` | JMAP session URL (null if IMAP) |
| `smtp_host` | `TEXT` | Outbound SMTP host |
| `smtp_port` | `INTEGER` | |
| `smtp_security` | `TEXT` | `tls`, `starttls`, `none` |
| `smtp_auth_type` | `TEXT` | Usually matches `auth_type` |
| `smtp_credential` | `TEXT` | Encrypted; may be same as `credential` |
| `auto_config_source` | `TEXT` | How config was discovered: `autoconfig`, `autodiscover`, `manual` |
| `is_active` | `BOOLEAN NOT NULL DEFAULT TRUE` | User can disable sync |
| `sync_enabled` | `BOOLEAN NOT NULL DEFAULT TRUE` | |
| `last_sync_at` | `TIMESTAMPTZ` | Last successful sync completion |

### 2.3 `folder`

Mailbox/folder metadata, per account.

| Column | Type | Notes |
|--------|------|-------|
| `id` | PK | |
| `account_id` | `TEXT NOT NULL` | FK → `mail_account.id` |
| `external_id` | `TEXT` | Server-side folder ID (JMAP mailbox id, IMAP folder encoded name) |
| `name` | `TEXT NOT NULL` | Display name |
| `parent_id` | `TEXT` | FK → `folder.id` for nesting |
| `role` | `TEXT` | Standard roles: `inbox`, `sent`, `drafts`, `trash`, `spam`, `archive`, `templates`, or NULL for user folders |
| `sort_order` | `INTEGER NOT NULL DEFAULT 0` | |
| `total_messages` | `INTEGER NOT NULL DEFAULT 0` | Denormalised count |
| `unread_messages` | `INTEGER NOT NULL DEFAULT 0` | Denormalised count |
| `sync_state` | `TEXT` | Opaque sync cursor (see §2.7) |

**Unique constraint:** `(account_id, external_id)`.

### 2.4 `message`

One row per email message. Stores metadata; full body is stored as a blob.

| Column | Type | Notes |
|--------|------|-------|
| `id` | PK | |
| `account_id` | `TEXT NOT NULL` | FK → `mail_account.id` |
| `folder_id` | `TEXT NOT NULL` | FK → `folder.id` |
| `external_id` | `TEXT` | Server-side message ID. IMAP: `{folder_id}:{uid}` — RFC 3501 UIDs are only unique within a mailbox, so the folder must scope the id (migration `0010` re-keyed existing rows). JMAP: opaque email id. |
| `thread_id` | `TEXT` | FK → `thread.id` |
| `message_id_header` | `TEXT` | RFC 5322 `Message-ID` header value |
| `subject` | `TEXT` | |
| `from_address` | `TEXT` | JSON: `{"name": "...", "email": "..."}` |
| `to_addresses` | `TEXT` | JSON array |
| `cc_addresses` | `TEXT` | JSON array |
| `bcc_addresses` | `TEXT` | JSON array |
| `reply_to` | `TEXT` | JSON array |
| `date` | `TIMESTAMPTZ` | `Date` header parsed |
| `received_at` | `TIMESTAMPTZ` | When server received it |
| `body_text` | `TEXT` | Plain-text body (for search) |
| `body_html` | `TEXT` | HTML body |
| `body_blob_path` | `TEXT` | Path to full MIME blob on disk (for large messages) |
| `is_read` | `BOOLEAN NOT NULL DEFAULT FALSE` | |
| `is_starred` | `BOOLEAN NOT NULL DEFAULT FALSE` | |
| `is_draft` | `BOOLEAN NOT NULL DEFAULT FALSE` | |
| `is_deleted` | `BOOLEAN NOT NULL DEFAULT FALSE` | Soft-delete |
| `flags` | `TEXT` | JSON object for additional IMAP/JMAP flags |
| `has_attachments` | `BOOLEAN NOT NULL DEFAULT FALSE` | |
| `size_bytes` | `INTEGER` | Approximate size |
| `in_reply_to` | `TEXT` | `In-Reply-To` header for threading |
| `references` | `TEXT` | `References` header |
| `labels` | `TEXT` | JSON array of label strings (JMAP keywords) |
| `snippet` | `TEXT` | First ~120 chars of text body for list view |

**Indexes:**

- `(account_id, folder_id, date)` — list view ordering
- `(account_id, external_id)` — sync lookup (unique)
- `(thread_id)` — thread view
- Full-text search on `subject`, `body_text`, `from_address` (via FTS5 on SQLite, `tsvector` on Postgres)

### 2.5 `attachment`

| Column | Type | Notes |
|--------|------|-------|
| `id` | PK | |
| `message_id` | `TEXT NOT NULL` | FK → `message.id` |
| `filename` | `TEXT` | |
| `content_type` | `TEXT` | MIME type |
| `size_bytes` | `INTEGER` | |
| `storage_path` | `TEXT NOT NULL` | Path on disk (under data dir) |
| `content_id` | `TEXT` | For inline images |
| `is_inline` | `BOOLEAN NOT NULL DEFAULT FALSE` | |

### 2.6 `thread`

Groups messages into a conversation thread.

| Column | Type | Notes |
|--------|------|-------|
| `id` | PK | |
| `account_id` | `TEXT NOT NULL` | FK → `mail_account.id` |
| `subject` | `TEXT` | Normalised subject (stripped Re:/Fwd:) |
| `snippet` | `TEXT` | Snippet from most recent message |
| `date` | `TIMESTAMPTZ` | Date of most recent message |
| `message_count` | `INTEGER NOT NULL DEFAULT 0` | |
| `unread_count` | `INTEGER NOT NULL DEFAULT 0` | |
| `is_starred` | `BOOLEAN NOT NULL DEFAULT FALSE` | Any message starred |
| `participants` | `TEXT` | JSON array of unique addresses |

### 2.7 `sync_cursor`

Tracks sync state per folder so sync can resume idempotently.

| Column | Type | Notes |
|--------|------|-------|
| `id` | PK | |
| `account_id` | `TEXT NOT NULL` | FK → `mail_account.id` |
| `folder_id` | `TEXT NOT NULL` | FK → `folder.id` |
| `protocol` | `TEXT NOT NULL` | `jmap` or `imap` |
| `cursor_type` | `TEXT NOT NULL` | `modseq`, `uidvalidity_uid`, `state_token` |
| `cursor_value` | `TEXT NOT NULL` | Opaque protocol-specific value |
| `updated_at` | `TIMESTAMPTZ` | |

**Unique constraint:** `(account_id, folder_id, cursor_type)`.

The cursor value is opaque to the application — it is whatever the protocol gives back (JMAP `state`, IMAP `UIDVALIDITY + HIGHESTMODSEQ`, etc.). The sync engine writes it after a successful batch and reads it on resume.

### 2.8 `contact` (CardDAV cache)

| Column | Type | Notes |
|--------|------|-------|
| `id` | PK | |
| `account_id` | `TEXT NOT NULL` | FK → `mail_account.id` |
| `external_id` | `TEXT` | CardDAV UID |
| `vcard_blob` | `TEXT` | Raw vCard data |
| `display_name` | `TEXT` | |
| `email_addresses` | `TEXT` | JSON array |
| `phone_numbers` | `TEXT` | JSON array |
| `organisation` | `TEXT` | |
| `photo_path` | `TEXT` | Local path to cached photo |
| `addressbook_url` | `TEXT` | Source addressbook |
| `etag` | `TEXT` | For sync change detection |

### 2.9 `calendar_event` (CalDAV cache)

| Column | Type | Notes |
|--------|------|-------|
| `id` | PK | |
| `account_id` | `TEXT NOT NULL` | FK → `mail_account.id` |
| `external_id` | `TEXT` | CalDAV UID |
| `icalendar_blob` | `TEXT` | Raw iCalendar data |
| `summary` | `TEXT` | Event title |
| `description` | `TEXT` | |
| `dtstart` | `TIMESTAMPTZ` | |
| `dtend` | `TIMESTAMPTZ` | |
| `location` | `TEXT` | |
| `is_all_day` | `BOOLEAN NOT NULL DEFAULT FALSE` | |
| `calendar_url` | `TEXT` | Source calendar |
| `etag` | `TEXT` | For sync change detection |
| `recurrence_rule` | `TEXT` | RRULE string |

---

## 3. Encrypted credentials

Mail-account credentials (`credential`, `smtp_credential`) are stored as **encrypted JSON blobs** using AES-256-GCM. The user's TOTP secret (`lyra_user.totp_secret`) is encrypted the same way.

### 3.1 Key hierarchy

1. A **master key** comes from the `LYRA_MASTER_KEY` environment variable (required, 32+ bytes; boot fails closed without it).
2. A per-user **key-encryption key** (KEK) is derived from the master key via HKDF-SHA256, with an info string bound to the user id (`lyra-user-kek:v1:<user_id>`).
3. A random 256-bit **data-encryption key** (DEK) is generated per user at bootstrap, wrapped with the KEK, and stored in `lyra_user.encrypted_dek`.
4. Credentials and the TOTP secret are encrypted with the DEK; the DEK is unwrapped on demand via the KEK.

This means account credentials are never encrypted directly under the master key, and each user's data is cryptographically separated.

**Pre-release databases** written before this hierarchy used the first 32 bytes of `LYRA_MASTER_KEY` (or a hardcoded dev default) as the AES key and left `encrypted_dek` NULL. The first `get_user_dek` for such a user mints a DEK and re-encrypts account passwords / TOTP secrets that still decrypt under that padded-key scheme. If decryption fails, re-enter the mailbox password in Settings (or reset the database).

### 3.2 Non-goals

- No HSM integration in v1.
- No key escrow or recovery — if the master key is lost, credentials are unrecoverable (by design: the user can re-add accounts).
- No plaintext passwords in logs, error messages, or debug output. Ever.

---

## 4. Ownership keys for future multi-user

Every table that contains user data has a `user_id` column (or, for `lyra_user`, the row itself), or a foreign-key chain that reaches `mail_account.user_id`. In v1 there is exactly one user, so direct `user_id` columns are always the same value. FK constraints are enforced from day one.

### 4.1 Ownership audit (v1 schema)

| Table | Ownership | Notes |
|-------|-----------|-------|
| `lyra_user` | row `id` | User root; holds wrapped DEK and TOTP |
| `mail_account` | `user_id` → `lyra_user.id` | Direct partition key for accounts |
| `folder` | `account_id` → `mail_account` | |
| `thread` | `account_id` → `mail_account` | |
| `message` | `account_id` → `mail_account` | |
| `attachment` | `message_id` → `message.account_id` → `mail_account` | |
| `sync_cursor` | `account_id` → `mail_account` | |
| `contact` | `account_id` → `mail_account` | PIM cache |
| `calendar` | `account_id` → `mail_account` | PIM cache |
| `calendar_event` | `account_id` → `mail_account` | PIM cache |
| `opengpg_key` | `user_id` → `lyra_user.id`; `account_id` (nullable) → `mail_account.id` | Direct partition key; identity keys additionally bind to one account (`0012_opengpg_key_account`) |
| `jobs` | `payload` JSON (`user_id` in `SyncAccount` etc.) | Kernel queue; no row-level `user_id` column yet |
| `schema_migrations` | — | System metadata; no user data |

No owned user-data table lacks a path to a user. Indirect chains always join through `mail_account.user_id` in API queries.

When multi-user lands:

- `mail_account.user_id` already partitions accounts.
- `folder`, `message`, `thread`, `sync_cursor` reach the user through `account_id → mail_account.user_id`.
- `contact`, `calendar`, and `calendar_event` reach the user through `account_id`.
- `opengpg_key.user_id` is already direct; `account_id` scopes identity keys per account and is nullable for shared contact keys.
- `jobs` may gain an explicit `user_id` column when multi-user job isolation is needed.

API endpoints extract `user_id` from the auth token; the query layer already accepts it.

---

## 5. Full-text search

| Engine | Strategy |
|--------|----------|
| SQLite | FTS5 virtual table on `message.subject`, `message.body_text`, `message.from_address`. Triggers keep it in sync. |
| PostgreSQL | `tsvector` column on `message` (generated or trigger-maintained) with a GIN index. |

The search module trait abstracts over both:

```rust
trait SearchIndex: Send + Sync {
    async fn index_message(&self, msg: &Message) -> Result<()>;
    async fn remove_message(&self, id: &str) -> Result<()>;
    async fn search(&self, query: &str, account_id: &str, limit: usize) -> Result<Vec<SearchHit>>;
}
```

---

## 6. Explicit non-goals

| Non-goal | Reason |
|----------|--------|
| Multi-user UX in v1 | Schema supports it; UI and API do not |
| Full-text search ranking tuned per-engine | Basic relevance is enough for v1; tune later |
| Encrypted database at rest | Filesystem-level encryption (LUKS, FileVault) is the operator's responsibility in v1 |
| Replication / HA | Single-instance v1; operators can use Postgres streaming replication externally |
| Schema versioning across major Lyra versions | Migrations are sequential; version-to-version upgrade path documented at v1 GA |
| Server-side rules / filtering | v1 relies on provider-side filtering; Lyra rules engine is a later feature |
| Attachment deduplication | Simple per-message storage; dedup can land later |

---

## 7. File-based blob storage

Large message bodies and attachments are stored on disk under `<data_dir>/blobs/`, organised by account and a content-addressable path (SHA-256 hash of the blob). The database stores only the relative path. This keeps the database small and avoids BLOB column limitations in SQLite.

Backup strategy: the entire `<data_dir>` (database + blobs) is the unit of backup.

---

## Related docs

- Product spec: `docs/product/2026-08-20-lyra-v1-product-spec.md`
- Engineering standards: `docs/specs/2026-08-20-lyra-engineering-standards.md`
- Sync and protocols: `docs/specs/2026-08-20-lyra-sync-and-protocols-spec.md`
