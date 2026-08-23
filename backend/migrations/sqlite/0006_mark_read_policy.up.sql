-- Per-user preference: when Lyra marks messages read (see auth preferences API).
ALTER TABLE lyra_user
    ADD COLUMN mark_read_policy TEXT NOT NULL DEFAULT 'on_open'
    CHECK (mark_read_policy IN ('on_open', 'on_scroll_end', 'manual'));
