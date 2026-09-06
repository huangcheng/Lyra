-- Add calendar table and calendar_event.calendar_id / status columns
-- See docs/specs/2026-08-20-lyra-data-model-spec.md


-- ─── Calendar ───────────────────────────────────────────────────────

CREATE TABLE IF NOT EXISTS calendar (
id VARCHAR(36) PRIMARY KEY NOT NULL,
account_id VARCHAR(36) NOT NULL,
external_id VARCHAR(190),
    name TEXT NOT NULL,
    color TEXT,
    description TEXT,
    timezone TEXT,
    calendar_url TEXT,
    etag TEXT,
    is_active TINYINT(1) NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL DEFAULT (DATE_FORMAT(UTC_TIMESTAMP(),'%Y-%m-%d %H:%M:%S')),
    updated_at TEXT NOT NULL DEFAULT (DATE_FORMAT(UTC_TIMESTAMP(),'%Y-%m-%d %H:%M:%S')),
    FOREIGN KEY (account_id) REFERENCES mail_account(id) ON DELETE CASCADE,
    UNIQUE(account_id, external_id)
);

CREATE INDEX idx_calendar_account_id ON calendar(account_id);

-- Prefer ADD COLUMN over table rewrite: SQLite cannot rename over an
-- existing table when FK enforcement / indexes leave residual names.
ALTER TABLE calendar_event ADD COLUMN calendar_id VARCHAR(36) REFERENCES calendar(id) ON DELETE SET NULL;
ALTER TABLE calendar_event ADD COLUMN status VARCHAR(36);

CREATE INDEX idx_calendar_event_calendar_id ON calendar_event(calendar_id);
