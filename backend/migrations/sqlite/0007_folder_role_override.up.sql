-- Local SPECIAL-USE role overrides (CHE-128). Sync updates `role` only;
-- `role_override` wins when set and is never cleared by IMAP/JMAP sync.
ALTER TABLE folder ADD COLUMN role_override TEXT;
