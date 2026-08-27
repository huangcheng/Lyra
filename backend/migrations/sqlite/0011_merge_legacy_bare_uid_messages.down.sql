-- Reverse the re-key (identical to 0010's down). Deleted duplicate rows
-- are not recoverable.

UPDATE message
SET external_id = SUBSTR(external_id, INSTR(external_id, ':') + 1)
WHERE external_id IS NOT NULL
  AND INSTR(external_id, ':') > 0
  AND account_id IN (
      SELECT id FROM mail_account
      WHERE COALESCE(receive_protocol, protocol) <> 'jmap'
  );
