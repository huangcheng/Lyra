-- Add calendar table and fix calendar_event schema
-- See docs/specs/2026-08-20-lyra-data-model-spec.md

PRAGMA foreign_keys = ON;

-- ─── Calendar ───────────────────────────────────────────────────────

CREATE TABLE IF NOT EXISTS calendar (
    id TEXT PRIMARY KEY NOT NULL,
    account_id TEXT NOT NULL,
    external_id TEXT,
    name TEXT NOT NULL,
    color TEXT,
    description TEXT,
    timezone TEXT,
    calendar_url TEXT,
    etag TEXT,
    is_active INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    FOREIGN KEY (account_id) REFERENCES mail_account(id) ON DELETE CASCADE,
    UNIQUE(account_id, external_id)
);

CREATE INDEX IF NOT EXISTS idx_calendar_account_id ON calendar(account_id);

-- Add calendar_id and status columns to calendar_event
-- SQLite doesn't support ALTER TABLE ADD COLUMN with FK, so we recreate

CREATE TABLE calendar_event_new (
    id TEXT PRIMARY KEY NOT NULL,
    account_id TEXT NOT NULL,
    calendar_id TEXT,
    external_id TEXT,
    icalendar_blob TEXT,
    summary TEXT,
    description TEXT,
    dtstart TEXT,
    dtend TEXT,
    location TEXT,
    is_all_day INTEGER NOT NULL DEFAULT 0,
    status TEXT,
    calendar_url TEXT,
    etag TEXT,
    recurrence_rule TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    FOREIGN KEY (account_id) REFERENCES mail_account(id) ON DELETE CASCADE,
    FOREIGN KEY (calendar_id) REFERENCES calendar(id) ON DELETE SET NULL
);

INSERT INTO calendar_event_new (id, account_id, external_id, icalendar_blob, summary, description, dtstart, dtend, location, is_all_day, calendar_url, etag, recurrence_rule, created_at, updated_at)
SELECT id, account_id, external_id, icalendar_blob, summary, description, dtstart, dtend, location, is_all_day, calendar_url, etag, recurrence_rule, created_at, updated_at
FROM calendar_event;

DROP TABLE calendar_event;
ALTER TABLE calendar_event_new RENAME TO calendar_event;

CREATE INDEX IF NOT EXISTS idx_calendar_event_account_id ON calendar_event(account_id);
CREATE INDEX IF NOT EXISTS idx_calendar_event_calendar_id ON calendar_event(calendar_id);
CREATE INDEX IF NOT EXISTS idx_calendar_event_external ON calendar_event(account_id, external_id);
