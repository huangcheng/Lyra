-- JMAP threadId: server-opaque string, no FK (thread.id is a local UUID).
ALTER TABLE message ADD COLUMN jmap_thread_id TEXT;
