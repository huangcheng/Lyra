-- SQLite cannot DROP COLUMN portably; down only removes the unique index.
DROP INDEX lyra_user_singleton;
