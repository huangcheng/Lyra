-- Per-user UI view-state as a JSON blob (selected account/folder, …).
-- Server-side so the mail view restores identically on any device.
ALTER TABLE lyra_user ADD COLUMN ui_state TEXT;
