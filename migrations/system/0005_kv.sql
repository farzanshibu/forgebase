-- 0005_kv: durable snapshot of the key-value engine (FB-31, ADR-0004-kv).
--
-- The KV engine serves reads and writes from memory. Changed keys are written
-- here in batches (write-behind) and on shutdown, and read back at start-up.
-- One row per key; `data` is the whole value serialised as JSON (binary parts
-- are base64). Only the KV service writes this table. KV data is cache-grade:
-- it is not a replacement for SQL records (SPEC 4.4).
CREATE TABLE _forgebase_kv_entries (
    tenant     TEXT    NOT NULL,
    namespace  TEXT    NOT NULL,
    key        TEXT    NOT NULL,
    type       TEXT    NOT NULL CHECK (type IN ('string', 'hash', 'list', 'set', 'zset', 'stream')),
    data       TEXT    NOT NULL CHECK (json_valid(data)),
    expires_at INTEGER,          -- unix milliseconds, NULL = never expires
    updated_at INTEGER NOT NULL, -- unix milliseconds of the last write
    PRIMARY KEY (tenant, namespace, key)
) STRICT, WITHOUT ROWID;

CREATE INDEX _forgebase_kv_entries_expires ON _forgebase_kv_entries (expires_at) WHERE expires_at IS NOT NULL;
