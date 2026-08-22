ALTER TABLE mail_account ADD COLUMN IF NOT EXISTS receive_protocol TEXT NOT NULL DEFAULT 'imap';
ALTER TABLE mail_account ADD COLUMN IF NOT EXISTS send_protocol TEXT NOT NULL DEFAULT 'smtp';
UPDATE mail_account SET receive_protocol = protocol WHERE protocol IN ('imap', 'jmap');
UPDATE mail_account SET send_protocol = 'smtp';

ALTER TABLE lyra_user ADD COLUMN IF NOT EXISTS sess_epoch INTEGER NOT NULL DEFAULT 0;
ALTER TABLE message ADD COLUMN IF NOT EXISTS snoozed_until TIMESTAMPTZ;

CREATE TABLE IF NOT EXISTS jobs (
    id TEXT PRIMARY KEY NOT NULL,
    kind TEXT NOT NULL,
    run_at TEXT NOT NULL,
    payload TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending',
    attempts INTEGER NOT NULL DEFAULT 0,
    last_error TEXT,
    created_at TEXT NOT NULL DEFAULT (NOW() AT TIME ZONE 'UTC')::TEXT,
    updated_at TEXT NOT NULL DEFAULT (NOW() AT TIME ZONE 'UTC')::TEXT
);
CREATE INDEX IF NOT EXISTS idx_jobs_due ON jobs(status, run_at);
