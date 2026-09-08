-- AI assistant chat history (P3-lite): multi-turn conversation
-- per user; cleared wholesale by DELETE /ai/chat.
CREATE TABLE ai_chat_message (
    id VARCHAR(36) PRIMARY KEY,
    user_id VARCHAR(36) NOT NULL REFERENCES lyra_user(id) ON DELETE CASCADE,
    role TEXT NOT NULL,
    content TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (DATE_FORMAT(UTC_TIMESTAMP(),'%Y-%m-%d %H:%M:%S'))
);
