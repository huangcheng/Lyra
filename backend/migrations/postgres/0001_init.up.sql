-- Lyra database schema for PostgreSQL
-- Initial migration: all core tables for v1
-- See docs/specs/2026-08-20-lyra-data-model-spec.md

-- Schema migration tracking
CREATE TABLE IF NOT EXISTS schema_migrations (
    version INTEGER PRIMARY KEY,
    applied_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- ─── User ────────────────────────────────────────────────────────────

CREATE TABLE lyra_user (
    id UUID PRIMARY KEY NOT NULL,  -- UUIDv7
    username TEXT NOT NULL UNIQUE,
    password_hash TEXT NOT NULL,
    totp_secret TEXT,              -- Encrypted; NULL if 2FA disabled
    totp_enabled BOOLEAN NOT NULL DEFAULT FALSE,
    display_name TEXT,
    locale TEXT NOT NULL DEFAULT 'en',
    -- Encrypted DEK for credential encryption (see data-model spec §3)
    encrypted_dek TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- ─── Mail Account ────────────────────────────────────────────────────

CREATE TABLE mail_account (
    id UUID PRIMARY KEY NOT NULL,
    user_id UUID NOT NULL,
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
    is_active BOOLEAN NOT NULL DEFAULT TRUE,
    sync_enabled BOOLEAN NOT NULL DEFAULT TRUE,
    last_sync_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    FOREIGN KEY (user_id) REFERENCES lyra_user(id) ON DELETE CASCADE
);

CREATE INDEX idx_mail_account_user_id ON mail_account(user_id);

-- ─── Folder ──────────────────────────────────────────────────────────

CREATE TABLE folder (
    id UUID PRIMARY KEY NOT NULL,
    account_id UUID NOT NULL,
    external_id TEXT,
    name TEXT NOT NULL,
    parent_id UUID,
    role TEXT,                     -- 'inbox', 'sent', 'drafts', 'trash', 'spam', 'archive', or NULL
    sort_order INTEGER NOT NULL DEFAULT 0,
    total_messages INTEGER NOT NULL DEFAULT 0,
    unread_messages INTEGER NOT NULL DEFAULT 0,
    sync_state TEXT,               -- Opaque sync cursor
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    FOREIGN KEY (account_id) REFERENCES mail_account(id) ON DELETE CASCADE,
    FOREIGN KEY (parent_id) REFERENCES folder(id) ON DELETE SET NULL,
    UNIQUE(account_id, external_id)
);

CREATE INDEX idx_folder_account_id ON folder(account_id);

-- ─── Thread ──────────────────────────────────────────────────────────

CREATE TABLE thread (
    id UUID PRIMARY KEY NOT NULL,
    account_id UUID NOT NULL,
    subject TEXT,
    snippet TEXT,
    date TIMESTAMPTZ,
    message_count INTEGER NOT NULL DEFAULT 0,
    unread_count INTEGER NOT NULL DEFAULT 0,
    is_starred BOOLEAN NOT NULL DEFAULT FALSE,
    participants JSONB,            -- JSON array of unique addresses
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    FOREIGN KEY (account_id) REFERENCES mail_account(id) ON DELETE CASCADE
);

CREATE INDEX idx_thread_account_id ON thread(account_id);
CREATE INDEX idx_thread_date ON thread(account_id, date);

-- ─── Message ─────────────────────────────────────────────────────────

CREATE TABLE message (
    id UUID PRIMARY KEY NOT NULL,
    account_id UUID NOT NULL,
    folder_id UUID NOT NULL,
    external_id TEXT,
    thread_id UUID,
    message_id_header TEXT,        -- RFC 5322 Message-ID
    subject TEXT,
    from_address JSONB,            -- {"name": "...", "email": "..."}
    to_addresses JSONB,            -- JSON array
    cc_addresses JSONB,            -- JSON array
    bcc_addresses JSONB,           -- JSON array
    reply_to JSONB,                -- JSON array
    date TIMESTAMPTZ,
    received_at TIMESTAMPTZ,
    body_text TEXT,                -- Plain-text for search
    body_html TEXT,                -- HTML body
    body_blob_path TEXT,           -- Path to full MIME blob on disk
    is_read BOOLEAN NOT NULL DEFAULT FALSE,
    is_starred BOOLEAN NOT NULL DEFAULT FALSE,
    is_draft BOOLEAN NOT NULL DEFAULT FALSE,
    is_deleted BOOLEAN NOT NULL DEFAULT FALSE,
    flags JSONB,                   -- Additional flags
    has_attachments BOOLEAN NOT NULL DEFAULT FALSE,
    size_bytes BIGINT,
    in_reply_to TEXT,
    references_headers TEXT,       -- References header (renamed to avoid SQL keyword)
    labels JSONB,                  -- Array of label strings
    snippet TEXT,                  -- First ~120 chars for list view
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
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
    id UUID PRIMARY KEY NOT NULL,
    message_id UUID NOT NULL,
    filename TEXT,
    content_type TEXT,
    size_bytes BIGINT,
    storage_path TEXT NOT NULL,
    content_id TEXT,
    is_inline BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    FOREIGN KEY (message_id) REFERENCES message(id) ON DELETE CASCADE
);

CREATE INDEX idx_attachment_message_id ON attachment(message_id);

-- ─── Sync Cursor ─────────────────────────────────────────────────────

CREATE TABLE sync_cursor (
    id UUID PRIMARY KEY NOT NULL,
    account_id UUID NOT NULL,
    folder_id UUID NOT NULL,
    protocol TEXT NOT NULL,        -- 'jmap' or 'imap'
    cursor_type TEXT NOT NULL,     -- 'modseq', 'uidvalidity_uid', 'state_token'
    cursor_value TEXT NOT NULL,
    updated_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    FOREIGN KEY (account_id) REFERENCES mail_account(id) ON DELETE CASCADE,
    FOREIGN KEY (folder_id) REFERENCES folder(id) ON DELETE CASCADE,
    UNIQUE(account_id, folder_id, cursor_type)
);

-- ─── Contact (CardDAV cache) ─────────────────────────────────────────

CREATE TABLE contact (
    id UUID PRIMARY KEY NOT NULL,
    account_id UUID NOT NULL,
    external_id TEXT,
    vcard_blob TEXT,
    display_name TEXT,
    email_addresses JSONB,         -- JSON array
    phone_numbers JSONB,           -- JSON array
    organisation TEXT,
    photo_path TEXT,
    addressbook_url TEXT,
    etag TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    FOREIGN KEY (account_id) REFERENCES mail_account(id) ON DELETE CASCADE
);

CREATE INDEX idx_contact_account_id ON contact(account_id);
CREATE INDEX idx_contact_external ON contact(account_id, external_id);

-- ─── Calendar Event (CalDAV cache) ───────────────────────────────────

CREATE TABLE calendar_event (
    id UUID PRIMARY KEY NOT NULL,
    account_id UUID NOT NULL,
    external_id TEXT,
    icalendar_blob TEXT,
    summary TEXT,
    description TEXT,
    dtstart TIMESTAMPTZ,
    dtend TIMESTAMPTZ,
    location TEXT,
    is_all_day BOOLEAN NOT NULL DEFAULT FALSE,
    calendar_url TEXT,
    etag TEXT,
    recurrence_rule TEXT,          -- RRULE string
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    FOREIGN KEY (account_id) REFERENCES mail_account(id) ON DELETE CASCADE
);

CREATE INDEX idx_calendar_event_account_id ON calendar_event(account_id);
CREATE INDEX idx_calendar_event_external ON calendar_event(account_id, external_id);
