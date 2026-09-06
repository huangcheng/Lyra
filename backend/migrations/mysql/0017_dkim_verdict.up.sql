-- DKIM verification verdict, computed lazily at view time. NULL status means
-- the message has never been verified; timestamps are UTC text like the rest
-- of the schema.
ALTER TABLE message ADD COLUMN dkim_status VARCHAR(36);
ALTER TABLE message ADD COLUMN dkim_sdid VARCHAR(36);
ALTER TABLE message ADD COLUMN dkim_auid VARCHAR(36);
ALTER TABLE message ADD COLUMN dkim_selector VARCHAR(36);
ALTER TABLE message ADD COLUMN dkim_algorithm VARCHAR(36);
ALTER TABLE message ADD COLUMN dkim_signed_headers VARCHAR(36);
ALTER TABLE message ADD COLUMN dkim_warnings VARCHAR(36);
ALTER TABLE message ADD COLUMN dkim_signed_at VARCHAR(36);
ALTER TABLE message ADD COLUMN dkim_expires_at VARCHAR(36);
