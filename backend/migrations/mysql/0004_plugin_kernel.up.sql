ALTER TABLE mail_account ADD COLUMN receive_protocol VARCHAR(36) NOT NULL DEFAULT 'imap';
ALTER TABLE mail_account ADD COLUMN send_protocol VARCHAR(36) NOT NULL DEFAULT 'smtp';
UPDATE mail_account SET receive_protocol = protocol WHERE protocol IN ('imap', 'jmap');
UPDATE mail_account SET send_protocol = 'smtp';

ALTER TABLE lyra_user ADD COLUMN sess_epoch BIGINT NOT NULL DEFAULT 0;
ALTER TABLE message ADD COLUMN snoozed_until VARCHAR(36);

CREATE TABLE IF NOT EXISTS jobs (
id VARCHAR(36) PRIMARY KEY NOT NULL,
    kind TEXT NOT NULL,
    run_at VARCHAR(255) NOT NULL,
    payload TEXT NOT NULL,
    status VARCHAR(255) NOT NULL DEFAULT 'pending',
    attempts BIGINT NOT NULL DEFAULT 0,
    last_error TEXT,
    created_at TEXT NOT NULL DEFAULT (DATE_FORMAT(UTC_TIMESTAMP(),'%Y-%m-%d %H:%M:%S')),
    updated_at TEXT NOT NULL DEFAULT (DATE_FORMAT(UTC_TIMESTAMP(),'%Y-%m-%d %H:%M:%S'))
);
CREATE INDEX idx_jobs_due ON jobs(status, run_at);
