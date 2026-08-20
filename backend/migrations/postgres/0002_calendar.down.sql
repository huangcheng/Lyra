-- Revert calendar table changes

ALTER TABLE calendar_event DROP COLUMN IF EXISTS calendar_id;
ALTER TABLE calendar_event DROP COLUMN IF EXISTS status;
DROP INDEX IF EXISTS idx_calendar_event_calendar_id;
DROP INDEX IF EXISTS idx_calendar_account_id;
DROP TABLE IF EXISTS calendar;
