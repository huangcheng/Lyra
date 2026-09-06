-- Revert PIM URL columns (SQLite cannot DROP COLUMN portably — leave no-op note)
-- For fresh DBs, down is only used in tests; recreate from 0001+0002 if needed.
SELECT 1;
