-- DKIM verification verdict, computed lazily at view time. NULL status means
-- the message has never been verified.
ALTER TABLE message ADD COLUMN dkim_status TEXT;
ALTER TABLE message ADD COLUMN dkim_sdid TEXT;
ALTER TABLE message ADD COLUMN dkim_auid TEXT;
ALTER TABLE message ADD COLUMN dkim_selector TEXT;
ALTER TABLE message ADD COLUMN dkim_algorithm TEXT;
ALTER TABLE message ADD COLUMN dkim_signed_headers TEXT;
ALTER TABLE message ADD COLUMN dkim_warnings TEXT;
ALTER TABLE message ADD COLUMN dkim_signed_at TIMESTAMPTZ;
ALTER TABLE message ADD COLUMN dkim_expires_at TIMESTAMPTZ;
