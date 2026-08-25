-- Full-text search index (CHE-108): tsvector + GIN on message.

ALTER TABLE message ADD COLUMN search_vector tsvector;

CREATE OR REPLACE FUNCTION message_search_vector_update() RETURNS trigger AS $$
BEGIN
    NEW.search_vector :=
        setweight(to_tsvector('simple', coalesce(NEW.subject, '')), 'A') ||
        setweight(to_tsvector('simple', coalesce(NEW.body_text, '')), 'B') ||
        setweight(to_tsvector('simple', coalesce(NEW.from_address::text, '')), 'A');
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER message_search_vector_update
    BEFORE INSERT OR UPDATE OF subject, body_text, from_address ON message
    FOR EACH ROW
    EXECUTE PROCEDURE message_search_vector_update();

CREATE INDEX idx_message_search_vector ON message USING GIN (search_vector);

UPDATE message
SET search_vector =
    setweight(to_tsvector('simple', coalesce(subject, '')), 'A') ||
    setweight(to_tsvector('simple', coalesce(body_text, '')), 'B') ||
    setweight(to_tsvector('simple', coalesce(from_address::text, '')), 'A');
