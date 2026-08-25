-- OpenGPG keyring (CHE-63 / opengpg-spec P1).
-- Migration number is 0008: 0007 already used for folder_role_override.
CREATE TABLE opengpg_key (
    id UUID PRIMARY KEY NOT NULL,
    user_id UUID NOT NULL REFERENCES lyra_user(id) ON DELETE CASCADE,
    fingerprint TEXT NOT NULL,
    primary_email TEXT NOT NULL,
    emails JSONB NOT NULL DEFAULT '[]'::jsonb,
    is_secret BOOLEAN NOT NULL,
    is_primary BOOLEAN NOT NULL DEFAULT FALSE,
    revoked BOOLEAN NOT NULL DEFAULT FALSE,
    key_data TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE UNIQUE INDEX idx_opengpg_key_user_fp ON opengpg_key (user_id, fingerprint);
CREATE INDEX idx_opengpg_key_user_email ON opengpg_key (user_id, primary_email);
