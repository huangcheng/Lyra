-- Single-user invariant, enforced at the database level.
-- `singleton` is always 1 (DEFAULT + CHECK) and the unique index rejects a
-- second row, so concurrent bootstrap races cannot create two owners even
-- when both pass the handler's has_any_user fast-path check.
ALTER TABLE lyra_user ADD COLUMN singleton INTEGER NOT NULL DEFAULT 1 CHECK (singleton = 1);
CREATE UNIQUE INDEX lyra_user_singleton ON lyra_user(singleton);
