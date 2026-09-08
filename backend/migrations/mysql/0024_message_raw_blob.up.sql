-- Backup export needs the raw RFC822 bytes; they live in the blob store and
-- this column records the content-addressed relative path once fetched.
ALTER TABLE message ADD COLUMN raw_blob_path TEXT;
