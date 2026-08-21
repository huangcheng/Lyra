-- Add calendar table and calendar_event.calendar_id / status columns
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

-- Prefer ADD COLUMN over table rewrite: SQLite cannot rename over an
-- existing table when FK enforcement / indexes leave residual names.
ALTER TABLE calendar_event ADD COLUMN calendar_id TEXT REFERENCES calendar(id) ON DELETE SET NULL;
ALTER TABLE calendar_event ADD COLUMN status TEXT;

CREATE INDEX IF NOT EXISTS idx_calendar_event_calendar_id ON calendar_event(calendar_id);
