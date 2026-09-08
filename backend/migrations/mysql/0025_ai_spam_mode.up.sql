-- Spam assist mode (P4): off | suggest | auto
ALTER TABLE ai_settings ADD COLUMN spam_mode TEXT NOT NULL;
