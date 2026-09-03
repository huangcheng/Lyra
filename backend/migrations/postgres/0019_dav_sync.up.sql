-- DAV sync state: per-collection RFC 6578 tokens + per-account cursor.
-- (contact.etag / calendar_event.etag already exist from 0001.)
ALTER TABLE calendar ADD COLUMN IF NOT EXISTS sync_token TEXT;

CREATE TABLE IF NOT EXISTS dav_cursor (
    account_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    token TEXT,
    PRIMARY KEY (account_id, kind)
);
