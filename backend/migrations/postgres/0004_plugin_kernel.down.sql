DROP TABLE IF EXISTS jobs;
ALTER TABLE message DROP COLUMN IF EXISTS snoozed_until;
ALTER TABLE lyra_user DROP COLUMN IF EXISTS sess_epoch;
ALTER TABLE mail_account DROP COLUMN IF EXISTS send_protocol;
ALTER TABLE mail_account DROP COLUMN IF EXISTS receive_protocol;
