-- DAV sync state (contact.etag / calendar_event.etag already exist from 0001).
ALTER TABLE calendar ADD COLUMN sync_token VARCHAR(36);

CREATE TABLE dav_cursor (
account_id VARCHAR(36) NOT NULL,
    kind VARCHAR(255) NOT NULL,
    token TEXT,
    PRIMARY KEY (account_id, kind)
);
