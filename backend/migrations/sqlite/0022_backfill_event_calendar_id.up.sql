-- Link calendar events to their calendar rows. Events synced before
-- ensure_calendar worked on PostgreSQL (raw text account binds) carry
-- calendar_url but a NULL calendar_id, which the list API filters on.
UPDATE calendar_event
SET calendar_id = (
    SELECT c.id
    FROM calendar c
    WHERE c.calendar_url = calendar_event.calendar_url
      AND c.account_id = calendar_event.account_id
)
WHERE calendar_id IS NULL;
