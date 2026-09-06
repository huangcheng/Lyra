-- Merge leftover bare-UID IMAP rows into their folder-scoped twins
-- (MySQL form: multi-table DELETE for the self-join, CONCAT instead of ||).

DELETE m
FROM message m
JOIN message twin
  ON twin.account_id = m.account_id
 AND twin.folder_id = m.folder_id
 AND twin.external_id = CONCAT(m.folder_id, ':', m.external_id)
WHERE m.external_id IS NOT NULL
  AND m.external_id NOT LIKE '%:%'
  AND m.account_id IN (
      SELECT id FROM mail_account
      WHERE COALESCE(receive_protocol, protocol) <> 'jmap'
  );

UPDATE message
SET external_id = CONCAT(folder_id, ':', external_id)
WHERE external_id IS NOT NULL
  AND external_id NOT LIKE '%:%'
  AND account_id IN (
      SELECT id FROM mail_account
      WHERE COALESCE(receive_protocol, protocol) <> 'jmap'
  );
