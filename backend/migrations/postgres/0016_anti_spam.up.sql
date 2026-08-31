-- Anti-spam: per-user settings, sender lists, and a per-message verdict so
-- the post-sync filter only judges each message once.
CREATE TABLE spam_settings (
    user_id UUID PRIMARY KEY REFERENCES lyra_user(id) ON DELETE CASCADE,
    enabled BOOLEAN NOT NULL DEFAULT FALSE,
    learn BOOLEAN NOT NULL DEFAULT TRUE,
    auto_delete BOOLEAN NOT NULL DEFAULT FALSE,
    sensitivity TEXT NOT NULL DEFAULT 'standard',
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE TABLE spam_sender (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES lyra_user(id) ON DELETE CASCADE,
    list TEXT NOT NULL CHECK (list IN ('blocked', 'allowed')),
    email TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (user_id, list, email)
);
ALTER TABLE message ADD COLUMN spam_verdict TEXT;
