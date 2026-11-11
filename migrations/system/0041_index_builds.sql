-- FB-173: per-set state of index builds on a sharded cluster (ADR-0029 §3.7).
--
-- Shards need PostgreSQL; on SQLite the table stays empty and exists so
-- both schemas stay identical (schema-parity test).
CREATE TABLE _forgebase_index_builds (
    index_name TEXT NOT NULL REFERENCES _forgebase_indexes(name) ON DELETE CASCADE,
    set_id     TEXT NOT NULL REFERENCES _forgebase_db_sets(id) ON DELETE CASCADE,
    state      TEXT NOT NULL CHECK (state IN ('building', 'ready', 'failed')),
    error      TEXT,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (index_name, set_id)
) STRICT;
CREATE INDEX _forgebase_index_builds_set ON _forgebase_index_builds (set_id);
