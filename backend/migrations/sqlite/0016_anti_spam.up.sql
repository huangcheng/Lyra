-- Anti-spam: per-user settings, sender lists, and a per-message verdict so
-- the post-sync filter only judges each message once.
CREATE TABLE spam_settings (
    user_id TEXT PRIMARY KEY REFERENCES lyra_user(id) ON DELETE CASCADE,
    enabled INTEGER NOT NULL DEFAULT 0,
    learn INTEGER NOT NULL DEFAULT 1,
    auto_delete INTEGER NOT NULL DEFAULT 0,
    sensitivity TEXT NOT NULL DEFAULT 'standard',
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE TABLE spam_sender (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES lyra_user(id) ON DELETE CASCADE,
    list TEXT NOT NULL CHECK (list IN ('blocked', 'allowed')),
    email TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE (user_id, list, email)
);
ALTER TABLE message ADD COLUMN spam_verdict TEXT;
