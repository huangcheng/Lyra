-- Lyra database schema for SQLite
-- Initial migration: all core tables for v1
-- See docs/specs/2026-08-20-lyra-data-model-spec.md

-- SQLite requires explicit foreign key enforcement
PRAGMA foreign_keys = ON;

-- Schema migration tracking
CREATE TABLE IF NOT EXISTS schema_migrations (
    version INTEGER PRIMARY KEY,
    applied_at TEXT NOT NULL DEFAULT (datetime('now'))
);

-- ─── User ────────────────────────────────────────────────────────────

CREATE TABLE lyra_user (
    id TEXT PRIMARY KEY NOT NULL,  -- UUIDv7
    username TEXT NOT NULL UNIQUE,
    password_hash TEXT NOT NULL,
    totp_secret TEXT,              -- Encrypted; NULL if 2FA disabled
    totp_enabled INTEGER NOT NULL DEFAULT 0,  -- BOOLEAN as INTEGER
    display_name TEXT,
    locale TEXT NOT NULL DEFAULT 'en',
    -- Encrypted DEK for credential encryption (see data-model spec §3)
    encrypted_dek TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

-- ─── Mail Account ────────────────────────────────────────────────────

CREATE TABLE mail_account (
    id TEXT PRIMARY KEY NOT NULL,
    user_id TEXT NOT NULL,
    display_name TEXT,
    email_address TEXT NOT NULL,
    protocol TEXT NOT NULL,        -- 'jmap' or 'imap'
    auth_type TEXT NOT NULL,       -- 'password', 'oauth2', 'app_password'
    credential TEXT NOT NULL,      -- Encrypted JSON blob
    imap_host TEXT,
    imap_port INTEGER,
    imap_security TEXT,            -- 'tls', 'starttls', 'none'
    jmap_base_url TEXT,
    smtp_host TEXT,
    smtp_port INTEGER,
    smtp_security TEXT,
    smtp_auth_type TEXT,
    smtp_credential TEXT,          -- Encrypted
    auto_config_source TEXT,       -- 'autoconfig', 'autodiscover', 'manual'
    is_active INTEGER NOT NULL DEFAULT 1,
    sync_enabled INTEGER NOT NULL DEFAULT 1,
    last_sync_at TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    FOREIGN KEY (user_id) REFERENCES lyra_user(id) ON DELETE CASCADE
);

CREATE INDEX idx_mail_account_user_id ON mail_account(user_id);

-- ─── Folder ──────────────────────────────────────────────────────────

CREATE TABLE folder (
    id TEXT PRIMARY KEY NOT NULL,
    account_id TEXT NOT NULL,
    external_id TEXT,
    name TEXT NOT NULL,
    parent_id TEXT,
    role TEXT,                     -- 'inbox', 'sent', 'drafts', 'trash', 'spam', 'archive', or NULL
    sort_order INTEGER NOT NULL DEFAULT 0,
    total_messages INTEGER NOT NULL DEFAULT 0,
    unread_messages INTEGER NOT NULL DEFAULT 0,
    sync_state TEXT,               -- Opaque sync cursor
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    FOREIGN KEY (account_id) REFERENCES mail_account(id) ON DELETE CASCADE,
    FOREIGN KEY (parent_id) REFERENCES folder(id) ON DELETE SET NULL,
    UNIQUE(account_id, external_id)
);

CREATE INDEX idx_folder_account_id ON folder(account_id);

-- ─── Thread ──────────────────────────────────────────────────────────

CREATE TABLE thread (
    id TEXT PRIMARY KEY NOT NULL,
    account_id TEXT NOT NULL,
    subject TEXT,
    snippet TEXT,
    date TEXT,                     -- ISO-8601 datetime
    message_count INTEGER NOT NULL DEFAULT 0,
    unread_count INTEGER NOT NULL DEFAULT 0,
    is_starred INTEGER NOT NULL DEFAULT 0,
    participants TEXT,             -- JSON array of unique addresses
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    FOREIGN KEY (account_id) REFERENCES mail_account(id) ON DELETE CASCADE
);

CREATE INDEX idx_thread_account_id ON thread(account_id);
CREATE INDEX idx_thread_date ON thread(account_id, date);

-- ─── Message ─────────────────────────────────────────────────────────

CREATE TABLE message (
    id TEXT PRIMARY KEY NOT NULL,
    account_id TEXT NOT NULL,
    folder_id TEXT NOT NULL,
    external_id TEXT,
    thread_id TEXT,
    message_id_header TEXT,        -- RFC 5322 Message-ID
    subject TEXT,
    from_address TEXT,             -- JSON: {"name": "...", "email": "..."}
    to_addresses TEXT,             -- JSON array
    cc_addresses TEXT,             -- JSON array
    bcc_addresses TEXT,            -- JSON array
    reply_to TEXT,                 -- JSON array
    date TEXT,                     -- ISO-8601 datetime
    received_at TEXT,              -- ISO-8601 datetime
    body_text TEXT,                -- Plain-text for search
    body_html TEXT,                -- HTML body
    body_blob_path TEXT,           -- Path to full MIME blob on disk
    is_read INTEGER NOT NULL DEFAULT 0,
    is_starred INTEGER NOT NULL DEFAULT 0,
    is_draft INTEGER NOT NULL DEFAULT 0,
    is_deleted INTEGER NOT NULL DEFAULT 0,
    flags TEXT,                    -- JSON object for additional flags
    has_attachments INTEGER NOT NULL DEFAULT 0,
    size_bytes INTEGER,
    in_reply_to TEXT,
    references_headers TEXT,       -- References header (renamed to avoid SQL keyword)
    labels TEXT,                   -- JSON array of label strings
    snippet TEXT,                  -- First ~120 chars for list view
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    FOREIGN KEY (account_id) REFERENCES mail_account(id) ON DELETE CASCADE,
    FOREIGN KEY (folder_id) REFERENCES folder(id) ON DELETE CASCADE,
    FOREIGN KEY (thread_id) REFERENCES thread(id) ON DELETE SET NULL,
    UNIQUE(account_id, external_id)
);

CREATE INDEX idx_message_list ON message(account_id, folder_id, date);
CREATE INDEX idx_message_thread ON message(thread_id);
CREATE INDEX idx_message_account_external ON message(account_id, external_id);

-- ─── Attachment ──────────────────────────────────────────────────────

CREATE TABLE attachment (
    id TEXT PRIMARY KEY NOT NULL,
    message_id TEXT NOT NULL,
    filename TEXT,
    content_type TEXT,
    size_bytes INTEGER,
    storage_path TEXT NOT NULL,
    content_id TEXT,
    is_inline INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    FOREIGN KEY (message_id) REFERENCES message(id) ON DELETE CASCADE
);

CREATE INDEX idx_attachment_message_id ON attachment(message_id);

-- ─── Sync Cursor ─────────────────────────────────────────────────────

CREATE TABLE sync_cursor (
    id TEXT PRIMARY KEY NOT NULL,
    account_id TEXT NOT NULL,
    folder_id TEXT NOT NULL,
    protocol TEXT NOT NULL,        -- 'jmap' or 'imap'
    cursor_type TEXT NOT NULL,     -- 'modseq', 'uidvalidity_uid', 'state_token'
    cursor_value TEXT NOT NULL,
    updated_at TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    FOREIGN KEY (account_id) REFERENCES mail_account(id) ON DELETE CASCADE,
    FOREIGN KEY (folder_id) REFERENCES folder(id) ON DELETE CASCADE,
    UNIQUE(account_id, folder_id, cursor_type)
);

-- ─── Contact (CardDAV cache) ─────────────────────────────────────────

CREATE TABLE contact (
    id TEXT PRIMARY KEY NOT NULL,
    account_id TEXT NOT NULL,
    external_id TEXT,
    vcard_blob TEXT,
    display_name TEXT,
    email_addresses TEXT,          -- JSON array
    phone_numbers TEXT,            -- JSON array
    organisation TEXT,
    photo_path TEXT,
    addressbook_url TEXT,
    etag TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    FOREIGN KEY (account_id) REFERENCES mail_account(id) ON DELETE CASCADE
);

CREATE INDEX idx_contact_account_id ON contact(account_id);
CREATE INDEX idx_contact_external ON contact(account_id, external_id);

-- ─── Calendar Event (CalDAV cache) ───────────────────────────────────

CREATE TABLE calendar_event (
    id TEXT PRIMARY KEY NOT NULL,
    account_id TEXT NOT NULL,
    external_id TEXT,
    icalendar_blob TEXT,
    summary TEXT,
    description TEXT,
    dtstart TEXT,
    dtend TEXT,
    location TEXT,
    is_all_day INTEGER NOT NULL DEFAULT 0,
    calendar_url TEXT,
    etag TEXT,
    recurrence_rule TEXT,          -- RRULE string
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    FOREIGN KEY (account_id) REFERENCES mail_account(id) ON DELETE CASCADE
);

CREATE INDEX idx_calendar_event_account_id ON calendar_event(account_id);
CREATE INDEX idx_calendar_event_external ON calendar_event(account_id, external_id);
