-- Revert calendar table changes

DROP INDEX IF EXISTS idx_calendar_event_calendar_id;
DROP INDEX IF EXISTS idx_calendar_account_id;
DROP TABLE IF EXISTS calendar;

CREATE TABLE calendar_event_old (
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
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    FOREIGN KEY (account_id) REFERENCES mail_account(id) ON DELETE CASCADE
);

INSERT INTO calendar_event_old SELECT id, account_id, external_id, icalendar_blob, summary, description, dtstart, dtend, location, is_all_day, calendar_url, etag, recurrence_rule, created_at, updated_at FROM calendar_event;
DROP TABLE calendar_event;
ALTER TABLE calendar_event_old RENAME TO calendar_event;

CREATE INDEX idx_calendar_event_account_id ON calendar_event(account_id);
CREATE INDEX idx_calendar_event_external ON calendar_event(account_id, external_id);
