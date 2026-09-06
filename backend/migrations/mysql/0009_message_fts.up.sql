-- Full-text search index (MySQL): one multi-column FULLTEXT index with the
-- ngram parser so CJK (zh) content tokenizes; MySQL maintains it from the
-- base table itself (no triggers, unlike the FTS5 build).

CREATE FULLTEXT INDEX message_fts_all ON message (subject, body_text, from_address) WITH PARSER ngram;
