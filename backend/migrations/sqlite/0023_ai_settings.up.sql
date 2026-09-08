-- AI assist P1: BYOK provider settings. api_key is a DEK-encrypted
-- JSON blob; features is a JSON object of per-feature flags (default off).
CREATE TABLE ai_settings (
    user_id TEXT PRIMARY KEY REFERENCES lyra_user(id) ON DELETE CASCADE,
    enabled INTEGER NOT NULL DEFAULT 0,
    dialect TEXT NOT NULL DEFAULT 'openai_chat',
    base_url TEXT NOT NULL DEFAULT '',
    model TEXT NOT NULL DEFAULT '',
    api_key TEXT NOT NULL DEFAULT '',
    features TEXT NOT NULL DEFAULT '{}',
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);
