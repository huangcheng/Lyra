-- Lyra database schema for PostgreSQL
-- Initial migration: portable TEXT / INTEGER types so the shared query layer
-- can bind and decode the same way as SQLite (see data-model spec §1.1).

-- Schema migration tracking
CREATE TABLE IF NOT EXISTS schema_migrations (
    version INTEGER PRIMARY KEY,
    applied_at TEXT NOT NULL DEFAULT (NOW() AT TIME ZONE 'UTC')
);

-- ─── User ────────────────────────────────────────────────────────────

CREATE TABLE lyra_user (
    id TEXT PRIMARY KEY NOT NULL,
    username TEXT NOT NULL UNIQUE,
    password_hash TEXT NOT NULL,
    totp_secret TEXT,
    totp_enabled INTEGER NOT NULL DEFAULT 0,
    display_name TEXT,
    locale TEXT NOT NULL DEFAULT 'en',
    encrypted_dek TEXT,
    created_at TEXT NOT NULL DEFAULT ((NOW() AT TIME ZONE 'UTC')::TEXT),
    updated_at TEXT NOT NULL DEFAULT ((NOW() AT TIME ZONE 'UTC')::TEXT)
);

-- ─── Mail Account ────────────────────────────────────────────────────

CREATE TABLE mail_account (
    id TEXT PRIMARY KEY NOT NULL,
    user_id TEXT NOT NULL,
    display_name TEXT,
    email_address TEXT NOT NULL,
    protocol TEXT NOT NULL,
    auth_type TEXT NOT NULL,
    credential TEXT NOT NULL,
    imap_host TEXT,
    imap_port INTEGER,
    imap_security TEXT,
    jmap_base_url TEXT,
    smtp_host TEXT,
    smtp_port INTEGER,
    smtp_security TEXT,
    smtp_auth_type TEXT,
    smtp_credential TEXT,
    auto_config_source TEXT,
    is_active INTEGER NOT NULL DEFAULT 1,
    sync_enabled INTEGER NOT NULL DEFAULT 1,
    last_sync_at TEXT,
    created_at TEXT NOT NULL DEFAULT ((NOW() AT TIME ZONE 'UTC')::TEXT),
    updated_at TEXT NOT NULL DEFAULT ((NOW() AT TIME ZONE 'UTC')::TEXT),
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
    role TEXT,
    sort_order INTEGER NOT NULL DEFAULT 0,
    total_messages INTEGER NOT NULL DEFAULT 0,
    unread_messages INTEGER NOT NULL DEFAULT 0,
    sync_state TEXT,
    created_at TEXT NOT NULL DEFAULT ((NOW() AT TIME ZONE 'UTC')::TEXT),
    updated_at TEXT NOT NULL DEFAULT ((NOW() AT TIME ZONE 'UTC')::TEXT),
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
    date TEXT,
    message_count INTEGER NOT NULL DEFAULT 0,
    unread_count INTEGER NOT NULL DEFAULT 0,
    is_starred INTEGER NOT NULL DEFAULT 0,
    participants TEXT,
    created_at TEXT NOT NULL DEFAULT ((NOW() AT TIME ZONE 'UTC')::TEXT),
    updated_at TEXT NOT NULL DEFAULT ((NOW() AT TIME ZONE 'UTC')::TEXT),
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
    message_id_header TEXT,
    subject TEXT,
    from_address TEXT,
    to_addresses TEXT,
    cc_addresses TEXT,
    bcc_addresses TEXT,
    reply_to TEXT,
    date TEXT,
    received_at TEXT,
    body_text TEXT,
    body_html TEXT,
    body_blob_path TEXT,
    is_read INTEGER NOT NULL DEFAULT 0,
    is_starred INTEGER NOT NULL DEFAULT 0,
    is_draft INTEGER NOT NULL DEFAULT 0,
    is_deleted INTEGER NOT NULL DEFAULT 0,
    flags TEXT,
    has_attachments INTEGER NOT NULL DEFAULT 0,
    size_bytes BIGINT,
    in_reply_to TEXT,
    references_headers TEXT,
    labels TEXT,
    snippet TEXT,
    created_at TEXT NOT NULL DEFAULT ((NOW() AT TIME ZONE 'UTC')::TEXT),
    updated_at TEXT NOT NULL DEFAULT ((NOW() AT TIME ZONE 'UTC')::TEXT),
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
    size_bytes BIGINT,
    storage_path TEXT NOT NULL,
    content_id TEXT,
    is_inline INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL DEFAULT ((NOW() AT TIME ZONE 'UTC')::TEXT),
    updated_at TEXT NOT NULL DEFAULT ((NOW() AT TIME ZONE 'UTC')::TEXT),
    FOREIGN KEY (message_id) REFERENCES message(id) ON DELETE CASCADE
);

CREATE INDEX idx_attachment_message_id ON attachment(message_id);

-- ─── Sync Cursor ─────────────────────────────────────────────────────

CREATE TABLE sync_cursor (
    id TEXT PRIMARY KEY NOT NULL,
    account_id TEXT NOT NULL,
    folder_id TEXT NOT NULL,
    protocol TEXT NOT NULL,
    cursor_type TEXT NOT NULL,
    cursor_value TEXT NOT NULL,
    updated_at TEXT,
    created_at TEXT NOT NULL DEFAULT ((NOW() AT TIME ZONE 'UTC')::TEXT),
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
    email_addresses TEXT,
    phone_numbers TEXT,
    organisation TEXT,
    photo_path TEXT,
    addressbook_url TEXT,
    etag TEXT,
    created_at TEXT NOT NULL DEFAULT ((NOW() AT TIME ZONE 'UTC')::TEXT),
    updated_at TEXT NOT NULL DEFAULT ((NOW() AT TIME ZONE 'UTC')::TEXT),
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
    recurrence_rule TEXT,
    created_at TEXT NOT NULL DEFAULT ((NOW() AT TIME ZONE 'UTC')::TEXT),
    updated_at TEXT NOT NULL DEFAULT ((NOW() AT TIME ZONE 'UTC')::TEXT),
    FOREIGN KEY (account_id) REFERENCES mail_account(id) ON DELETE CASCADE
);

CREATE INDEX idx_calendar_event_account_id ON calendar_event(account_id);
CREATE INDEX idx_calendar_event_external ON calendar_event(account_id, external_id);
