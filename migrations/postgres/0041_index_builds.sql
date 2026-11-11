-- FB-173: per-set state of index builds on a sharded cluster (ADR-0029 §3.7).
--
-- `CREATE INDEX CONCURRENTLY` cannot run inside the two-phase schema
-- transaction, so an index made with the index manager rolls out to each
-- shard on its own: one row per (index, shard) while it is `building`, then
-- `ready` or `failed` (with the error). No row means the index was built
-- with its collection (in the schema transaction or the catalog replay of a
-- new shard). Control's own copy is built with the catalog row.
CREATE TABLE _forgebase_index_builds (
    index_name text NOT NULL REFERENCES _forgebase_indexes(name) ON DELETE CASCADE DEFERRABLE INITIALLY IMMEDIATE,
    set_id     text NOT NULL REFERENCES _forgebase_db_sets(id) ON DELETE CASCADE DEFERRABLE INITIALLY IMMEDIATE,
    state      text NOT NULL CHECK (state IN ('building', 'ready', 'failed')),
    error      text,
    updated_at text NOT NULL,
    PRIMARY KEY (index_name, set_id)
);
CREATE INDEX _forgebase_index_builds_set ON _forgebase_index_builds (set_id);

-- System tables created after 0040 enable row-level security themselves
-- (FB-152: no policies, so only trusted sessions read them directly).
ALTER TABLE _forgebase_index_builds ENABLE ROW LEVEL SECURITY;
