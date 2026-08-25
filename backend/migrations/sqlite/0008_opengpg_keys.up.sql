-- OpenGPG keyring (CHE-63 / opengpg-spec P1).
-- Migration number is 0008: 0007 already used for folder_role_override.
CREATE TABLE opengpg_key (
    id TEXT PRIMARY KEY NOT NULL,
    user_id TEXT NOT NULL REFERENCES lyra_user(id) ON DELETE CASCADE,
    fingerprint TEXT NOT NULL,
    primary_email TEXT NOT NULL,
    emails TEXT NOT NULL DEFAULT '[]',
    is_secret INTEGER NOT NULL,
    is_primary INTEGER NOT NULL DEFAULT 0,
    revoked INTEGER NOT NULL DEFAULT 0,
    key_data TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE UNIQUE INDEX idx_opengpg_key_user_fp ON opengpg_key (user_id, fingerprint);
CREATE INDEX idx_opengpg_key_user_email ON opengpg_key (user_id, primary_email);
