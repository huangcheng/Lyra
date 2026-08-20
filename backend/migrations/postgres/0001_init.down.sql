-- Lyra PostgreSQL down migration
-- Drops all tables in reverse dependency order

DROP TABLE IF EXISTS calendar_event CASCADE;
DROP TABLE IF EXISTS contact CASCADE;
DROP TABLE IF EXISTS sync_cursor CASCADE;
DROP TABLE IF EXISTS attachment CASCADE;
DROP TABLE IF EXISTS message CASCADE;
DROP TABLE IF EXISTS thread CASCADE;
DROP TABLE IF EXISTS folder CASCADE;
DROP TABLE IF EXISTS mail_account CASCADE;
DROP TABLE IF EXISTS lyra_user CASCADE;
