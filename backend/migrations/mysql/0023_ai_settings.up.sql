-- AI assist P1: BYOK provider settings. api_key is a DEK-encrypted
-- JSON blob; features is a JSON object of per-feature flags (default off).
CREATE TABLE ai_settings (
    user_id VARCHAR(36) PRIMARY KEY REFERENCES lyra_user(id) ON DELETE CASCADE,
    enabled BIGINT NOT NULL DEFAULT 0,
    dialect TEXT NOT NULL DEFAULT ('openai_chat'),
    base_url TEXT NOT NULL,
    model TEXT NOT NULL,
    api_key TEXT NOT NULL,
    features TEXT NOT NULL,
    updated_at TEXT NOT NULL DEFAULT (DATE_FORMAT(UTC_TIMESTAMP(),'%Y-%m-%d %H:%M:%S'))
);
