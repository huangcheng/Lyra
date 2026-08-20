-- Add calendar table and fix calendar_event schema
-- See docs/specs/2026-08-20-lyra-data-model-spec.md

-- ─── Calendar ───────────────────────────────────────────────────────

CREATE TABLE IF NOT EXISTS calendar (
    id UUID PRIMARY KEY NOT NULL,
    account_id UUID NOT NULL,
    external_id TEXT,
    name TEXT NOT NULL,
    color TEXT,
    description TEXT,
    timezone TEXT,
    calendar_url TEXT,
    etag TEXT,
    is_active BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    FOREIGN KEY (account_id) REFERENCES mail_account(id) ON DELETE CASCADE,
    UNIQUE(account_id, external_id)
);

CREATE INDEX IF NOT EXISTS idx_calendar_account_id ON calendar(account_id);

-- Add calendar_id and status columns to calendar_event
ALTER TABLE calendar_event ADD COLUMN IF NOT EXISTS calendar_id UUID REFERENCES calendar(id) ON DELETE SET NULL;
ALTER TABLE calendar_event ADD COLUMN IF NOT EXISTS status TEXT;

CREATE INDEX IF NOT EXISTS idx_calendar_event_calendar_id ON calendar_event(calendar_id);
