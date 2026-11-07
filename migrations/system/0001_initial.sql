-- 0001_initial: ForgeBase system metadata.
--
-- System tables use the reserved `_forgebase_` prefix; user collections may
-- never start with an underscore. The migration bookkeeping table
-- (`_forgebase_migrations`) is created by the migrator itself, not here.

-- Instance-level key/value metadata (instance id, initialisation time, ...).
CREATE TABLE _forgebase_meta (
    key        TEXT NOT NULL PRIMARY KEY CHECK (length(key) BETWEEN 1 AND 255),
    value      TEXT NOT NULL,
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
) STRICT, WITHOUT ROWID;
