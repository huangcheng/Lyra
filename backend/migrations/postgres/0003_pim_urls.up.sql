-- PIM URLs for CardDAV / CalDAV sync

ALTER TABLE mail_account ADD COLUMN IF NOT EXISTS carddav_url TEXT;
ALTER TABLE mail_account ADD COLUMN IF NOT EXISTS caldav_url TEXT;
