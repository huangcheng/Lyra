-- Anti-spam: per-user settings, sender lists, and a per-message verdict so
-- the post-sync filter only judges each message once.
CREATE TABLE spam_settings (
user_id VARCHAR(36) PRIMARY KEY REFERENCES lyra_user(id) ON DELETE CASCADE,
    enabled BIGINT NOT NULL DEFAULT 0,
    learn BIGINT NOT NULL DEFAULT 1,
    auto_delete BIGINT NOT NULL DEFAULT 0,
    sensitivity TEXT NOT NULL DEFAULT ('standard'),
    updated_at TEXT NOT NULL DEFAULT (DATE_FORMAT(UTC_TIMESTAMP(),'%Y-%m-%d %H:%M:%S'))
);
CREATE TABLE spam_sender (
id VARCHAR(36) PRIMARY KEY,
user_id VARCHAR(36) NOT NULL REFERENCES lyra_user(id) ON DELETE CASCADE,
    list VARCHAR(255) NOT NULL CHECK (list IN ('blocked', 'allowed')),
    email VARCHAR(255) NOT NULL,
    created_at TEXT NOT NULL DEFAULT (DATE_FORMAT(UTC_TIMESTAMP(),'%Y-%m-%d %H:%M:%S')),
    UNIQUE (user_id, list, email)
);
ALTER TABLE message ADD COLUMN spam_verdict VARCHAR(36);
