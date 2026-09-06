-- Link calendar events to their calendar rows. Events synced before
-- ensure_calendar worked on PostgreSQL (raw text account binds) carry
-- calendar_url but a NULL calendar_id, which the list API filters on.
UPDATE calendar_event e
SET calendar_id = c.id
FROM calendar c
WHERE e.calendar_id IS NULL
  AND e.calendar_url = c.calendar_url
  AND e.account_id = c.account_id;
