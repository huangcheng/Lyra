-- Bind OpenGPG keys to a mail account (identity keys are per-account).
-- Rows that stay unbound remain shared contact/legacy keys.

ALTER TABLE opengpg_key ADD COLUMN account_id TEXT REFERENCES mail_account(id) ON DELETE SET NULL;

CREATE INDEX idx_opengpg_key_account ON opengpg_key (account_id);

-- Adopt existing keys whose primary email matches one of the user's
-- account addresses (first match wins); everything else stays unbound.
UPDATE opengpg_key
SET account_id = (
    SELECT ma.id
    FROM mail_account ma
    WHERE ma.user_id = opengpg_key.user_id
      AND lower(ma.email_address) = lower(opengpg_key.primary_email)
    LIMIT 1
)
WHERE account_id IS NULL;
