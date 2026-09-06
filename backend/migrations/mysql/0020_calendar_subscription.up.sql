-- ICS / webcal calendar subscriptions (user-owned, not mail_account).


CREATE TABLE IF NOT EXISTS calendar_subscription (
id VARCHAR(36) PRIMARY KEY NOT NULL,
user_id VARCHAR(36) NOT NULL,
    url VARCHAR(255) NOT NULL,
    name TEXT NOT NULL,
    color TEXT,
    etag TEXT,
    last_modified TEXT,
    last_fetched_at TEXT,
    last_error TEXT,
    is_active TINYINT(1) NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL DEFAULT (DATE_FORMAT(UTC_TIMESTAMP(),'%Y-%m-%d %H:%M:%S')),
    updated_at TEXT NOT NULL DEFAULT (DATE_FORMAT(UTC_TIMESTAMP(),'%Y-%m-%d %H:%M:%S')),
    FOREIGN KEY (user_id) REFERENCES lyra_user(id) ON DELETE CASCADE,
    UNIQUE (user_id, url)
);

CREATE INDEX idx_calendar_subscription_user
    ON calendar_subscription(user_id);

CREATE TABLE IF NOT EXISTS subscription_event (
id VARCHAR(36) PRIMARY KEY NOT NULL,
subscription_id VARCHAR(36) NOT NULL,
external_id VARCHAR(190),
    icalendar_blob TEXT,
    summary TEXT,
    description TEXT,
    dtstart VARCHAR(255),
    dtend TEXT,
    location TEXT,
    is_all_day TINYINT(1) NOT NULL DEFAULT 0,
    recurrence_rule TEXT,
    status TEXT,
    created_at TEXT NOT NULL DEFAULT (DATE_FORMAT(UTC_TIMESTAMP(),'%Y-%m-%d %H:%M:%S')),
    updated_at TEXT NOT NULL DEFAULT (DATE_FORMAT(UTC_TIMESTAMP(),'%Y-%m-%d %H:%M:%S')),
    FOREIGN KEY (subscription_id) REFERENCES calendar_subscription(id) ON DELETE CASCADE,
    UNIQUE (subscription_id, external_id)
);

CREATE INDEX idx_subscription_event_sub
    ON subscription_event(subscription_id);
CREATE INDEX idx_subscription_event_dtstart
    ON subscription_event(dtstart);
