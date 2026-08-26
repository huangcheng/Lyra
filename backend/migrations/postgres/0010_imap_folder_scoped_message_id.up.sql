-- Re-key IMAP message identity to be folder-scoped (RFC 3501 UIDs are only
-- unique within a mailbox; bare UIDs collided across folders on the
-- UNIQUE(account_id, external_id) upsert, silently absorbing messages).
--
-- JMAP rows keep their opaque ids (globally unique); only non-JMAP
-- (IMAP) accounts are prefixed with the row's folder_id.

UPDATE message
SET external_id = folder_id || ':' || external_id
WHERE external_id IS NOT NULL
  AND account_id IN (
      SELECT id FROM mail_account
      WHERE COALESCE(receive_protocol, protocol) <> 'jmap'
  );
