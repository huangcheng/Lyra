-- AI assistant chat history (P3-lite): multi-turn conversation
-- per user; cleared wholesale by DELETE /ai/chat.
CREATE TABLE ai_chat_message (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES lyra_user(id) ON DELETE CASCADE,
    role TEXT NOT NULL CHECK (role IN ('user', 'assistant')),
    content TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
