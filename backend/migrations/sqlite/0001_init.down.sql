-- Lyra SQLite down migration
-- Drops all tables in reverse dependency order

PRAGMA foreign_keys = ON;

DROP TABLE IF EXISTS calendar_event;
DROP TABLE IF EXISTS contact;
DROP TABLE IF EXISTS sync_cursor;
DROP TABLE IF EXISTS attachment;
DROP TABLE IF EXISTS message;
DROP TABLE IF EXISTS thread;
DROP TABLE IF EXISTS folder;
DROP TABLE IF EXISTS mail_account;
DROP TABLE IF EXISTS lyra_user;
