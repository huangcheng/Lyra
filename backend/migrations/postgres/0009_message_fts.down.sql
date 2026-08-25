DROP TRIGGER IF EXISTS message_search_vector_update ON message;
DROP FUNCTION IF EXISTS message_search_vector_update();
DROP INDEX IF EXISTS idx_message_search_vector;
ALTER TABLE message DROP COLUMN IF EXISTS search_vector;
