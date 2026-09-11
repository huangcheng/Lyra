-- Sender MUA self-identification (User-Agent / X-Mailer), shown as a "via …" chip.
ALTER TABLE message ADD COLUMN mailer TEXT;
