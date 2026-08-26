-- Revert folder-scoped IMAP message ids back to bare UIDs.
-- (Lossy if two folders held the same UID; the up data is gone.)

UPDATE message
SET external_id = SUBSTRING(external_id FROM POSITION(':' IN external_id) + 1)
WHERE external_id IS NOT NULL
  AND POSITION(':' IN external_id) > 0
  AND account_id IN (
      SELECT id FROM mail_account
      WHERE COALESCE(receive_protocol, protocol) <> 'jmap'
  );
