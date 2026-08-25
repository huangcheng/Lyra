-- Full-text search index (CHE-108): FTS5 on subject, body_text, from_address.

CREATE VIRTUAL TABLE message_fts USING fts5(
    message_id UNINDEXED,
    account_id UNINDEXED,
    subject,
    body_text,
    from_address
);

CREATE TRIGGER message_fts_ai AFTER INSERT ON message BEGIN
    INSERT INTO message_fts (message_id, account_id, subject, body_text, from_address)
    VALUES (
        NEW.id,
        NEW.account_id,
        COALESCE(NEW.subject, ''),
        COALESCE(NEW.body_text, ''),
        COALESCE(NEW.from_address, '')
    );
END;

CREATE TRIGGER message_fts_ad AFTER DELETE ON message BEGIN
    DELETE FROM message_fts WHERE message_id = OLD.id;
END;

CREATE TRIGGER message_fts_au AFTER UPDATE ON message BEGIN
    DELETE FROM message_fts WHERE message_id = OLD.id;
    INSERT INTO message_fts (message_id, account_id, subject, body_text, from_address)
    VALUES (
        NEW.id,
        NEW.account_id,
        COALESCE(NEW.subject, ''),
        COALESCE(NEW.body_text, ''),
        COALESCE(NEW.from_address, '')
    );
END;

INSERT INTO message_fts (message_id, account_id, subject, body_text, from_address)
SELECT id, account_id, COALESCE(subject, ''), COALESCE(body_text, ''), COALESCE(from_address, '')
FROM message;
