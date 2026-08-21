-- PIM URLs for CardDAV / CalDAV sync

ALTER TABLE mail_account ADD COLUMN carddav_url TEXT;
ALTER TABLE mail_account ADD COLUMN caldav_url TEXT;
