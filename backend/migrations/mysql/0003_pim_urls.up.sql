-- PIM URLs for CardDAV / CalDAV sync

ALTER TABLE mail_account ADD COLUMN carddav_url VARCHAR(36);
ALTER TABLE mail_account ADD COLUMN caldav_url VARCHAR(36);
