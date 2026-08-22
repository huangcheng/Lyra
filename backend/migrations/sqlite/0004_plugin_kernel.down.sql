-- SQLite cannot DROP COLUMN portably for receive_protocol / send_protocol /
-- sess_epoch / snoozed_until. Down only removes the jobs table.
DROP TABLE IF EXISTS jobs;
