-- ICS / webcal calendar subscriptions (user-owned, not mail_account).

PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS calendar_subscription (
    id TEXT PRIMARY KEY NOT NULL,
    user_id TEXT NOT NULL,
    url TEXT NOT NULL,
    name TEXT NOT NULL,
    color TEXT,
    etag TEXT,
    last_modified TEXT,
    last_fetched_at TEXT,
    last_error TEXT,
    is_active INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    FOREIGN KEY (user_id) REFERENCES lyra_user(id) ON DELETE CASCADE,
    UNIQUE (user_id, url)
);

CREATE INDEX IF NOT EXISTS idx_calendar_subscription_user
    ON calendar_subscription(user_id);

CREATE TABLE IF NOT EXISTS subscription_event (
    id TEXT PRIMARY KEY NOT NULL,
    subscription_id TEXT NOT NULL,
    external_id TEXT,
    icalendar_blob TEXT,
    summary TEXT,
    description TEXT,
    dtstart TEXT,
    dtend TEXT,
    location TEXT,
    is_all_day INTEGER NOT NULL DEFAULT 0,
    recurrence_rule TEXT,
    status TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    FOREIGN KEY (subscription_id) REFERENCES calendar_subscription(id) ON DELETE CASCADE,
    UNIQUE (subscription_id, external_id)
);

CREATE INDEX IF NOT EXISTS idx_subscription_event_sub
    ON subscription_event(subscription_id);
CREATE INDEX IF NOT EXISTS idx_subscription_event_dtstart
    ON subscription_event(dtstart);
