-- Merge leftover bare-UID IMAP rows into their folder-scoped twins.
--
-- 0010 re-keyed external_id to `folder_id:uid`, but a pre-0010 process
-- could still flush bare-UID rows during deploy overlap (observed 4s
-- after 0010 was applied), leaving two rows for the same message.
--
-- 1) Drop bare rows whose folder-scoped twin exists (twin is fresher).
-- 2) Re-key the remaining bare rows (same transform as 0010, now
--    conflict-free) so UID-scoped operations keep working.

DELETE FROM message
WHERE external_id IS NOT NULL
  AND external_id NOT LIKE '%:%'
  AND account_id IN (
      SELECT id FROM mail_account
      WHERE COALESCE(receive_protocol, protocol) <> 'jmap'
  )
  AND EXISTS (
      SELECT 1 FROM message twin
      WHERE twin.account_id = message.account_id
        AND twin.folder_id = message.folder_id
        AND twin.external_id = message.folder_id::text || ':' || message.external_id
  );

UPDATE message
SET external_id = folder_id::text || ':' || external_id
WHERE external_id IS NOT NULL
  AND external_id NOT LIKE '%:%'
  AND account_id IN (
      SELECT id FROM mail_account
      WHERE COALESCE(receive_protocol, protocol) <> 'jmap'
  );
