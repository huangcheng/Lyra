-- ICS / webcal calendar subscriptions (user-owned, not mail_account).

CREATE TABLE IF NOT EXISTS calendar_subscription (
    id UUID PRIMARY KEY NOT NULL,
    user_id UUID NOT NULL REFERENCES lyra_user(id) ON DELETE CASCADE,
    url TEXT NOT NULL,
    name TEXT NOT NULL,
    color TEXT,
    etag TEXT,
    last_modified TEXT,
    last_fetched_at TIMESTAMPTZ,
    last_error TEXT,
    is_active BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (user_id, url)
);

CREATE INDEX IF NOT EXISTS idx_calendar_subscription_user
    ON calendar_subscription(user_id);

CREATE TABLE IF NOT EXISTS subscription_event (
    id UUID PRIMARY KEY NOT NULL,
    subscription_id UUID NOT NULL REFERENCES calendar_subscription(id) ON DELETE CASCADE,
    external_id TEXT,
    icalendar_blob TEXT,
    summary TEXT,
    description TEXT,
    dtstart TEXT,
    dtend TEXT,
    location TEXT,
    is_all_day BOOLEAN NOT NULL DEFAULT FALSE,
    recurrence_rule TEXT,
    status TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (subscription_id, external_id)
);

CREATE INDEX IF NOT EXISTS idx_subscription_event_sub
    ON subscription_event(subscription_id);
CREATE INDEX IF NOT EXISTS idx_subscription_event_dtstart
    ON subscription_event(dtstart);
