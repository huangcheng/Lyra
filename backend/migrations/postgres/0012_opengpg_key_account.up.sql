-- Bind OpenGPG keys to a mail account (identity keys are per-account).
-- Rows that stay unbound remain shared contact/legacy keys.

ALTER TABLE opengpg_key ADD COLUMN account_id UUID REFERENCES mail_account(id) ON DELETE SET NULL;

CREATE INDEX idx_opengpg_key_account ON opengpg_key (account_id);

-- Adopt existing keys whose primary email matches one of the user's
-- account addresses; multi-match picks an arbitrary row (first match wins).
UPDATE opengpg_key
SET account_id = ma.id
FROM mail_account ma
WHERE ma.user_id = opengpg_key.user_id
  AND lower(ma.email_address) = lower(opengpg_key.primary_email)
  AND opengpg_key.account_id IS NULL;
