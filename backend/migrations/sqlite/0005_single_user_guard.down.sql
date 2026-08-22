-- SQLite cannot DROP COLUMN portably; down only removes the unique index.
DROP INDEX IF EXISTS lyra_user_singleton;
