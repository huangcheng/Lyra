-- OpenGPG keyring (CHE-63 / opengpg-spec P1).
-- Migration number is 0008: 0007 already used for folder_role_override.
CREATE TABLE opengpg_key (
id VARCHAR(36) PRIMARY KEY NOT NULL,
user_id VARCHAR(36) NOT NULL REFERENCES lyra_user(id) ON DELETE CASCADE,
    fingerprint VARCHAR(255) NOT NULL,
    primary_email VARCHAR(255) NOT NULL,
    emails TEXT NOT NULL DEFAULT ('[]'),
    is_secret TINYINT(1) NOT NULL,
    is_primary TINYINT(1) NOT NULL DEFAULT 0,
    revoked BIGINT NOT NULL DEFAULT 0,
    key_data TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (DATE_FORMAT(UTC_TIMESTAMP(),'%Y-%m-%d %H:%M:%S')),
    updated_at TEXT NOT NULL DEFAULT (DATE_FORMAT(UTC_TIMESTAMP(),'%Y-%m-%d %H:%M:%S'))
);

CREATE UNIQUE INDEX idx_opengpg_key_user_fp ON opengpg_key (user_id, fingerprint);
CREATE INDEX idx_opengpg_key_user_email ON opengpg_key (user_id, primary_email);
