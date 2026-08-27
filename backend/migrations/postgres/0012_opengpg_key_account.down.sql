DROP INDEX IF EXISTS idx_opengpg_key_account;
ALTER TABLE opengpg_key DROP COLUMN IF EXISTS account_id;
