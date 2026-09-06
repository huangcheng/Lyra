-- Rebuild without account_id (SQLite cannot drop FK columns in place).

CREATE TABLE opengpg_key_new (
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

INSERT INTO opengpg_key_new (
    id, user_id, fingerprint, primary_email, emails, is_secret,
    is_primary, revoked, key_data, created_at, updated_at
)
SELECT
    id, user_id, fingerprint, primary_email, emails, is_secret,
    is_primary, revoked, key_data, created_at, updated_at
FROM opengpg_key;

DROP TABLE opengpg_key;
ALTER TABLE opengpg_key_new RENAME TO opengpg_key;

CREATE UNIQUE INDEX idx_opengpg_key_user_fp ON opengpg_key (user_id, fingerprint);
CREATE INDEX idx_opengpg_key_user_email ON opengpg_key (user_id, primary_email);
