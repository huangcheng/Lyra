-- DAV sync state (contact.etag / calendar_event.etag already exist from 0001).
ALTER TABLE calendar ADD COLUMN sync_token TEXT;

CREATE TABLE dav_cursor (
    account_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    token TEXT,
    PRIMARY KEY (account_id, kind)
);
