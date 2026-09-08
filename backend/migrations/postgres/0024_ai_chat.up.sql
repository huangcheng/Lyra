-- AI assistant chat history (P3-lite): multi-turn conversation
-- per user; cleared wholesale by DELETE /ai/chat.
CREATE TABLE ai_chat_message (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES lyra_user(id) ON DELETE CASCADE,
    role TEXT NOT NULL CHECK (role IN ('user', 'assistant')),
    content TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
