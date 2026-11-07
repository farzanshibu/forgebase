-- 0002_data_core: catalog of the Phase 2 data core (FB-10, ADR-0003).
--
-- User collections are ordinary tables named after the collection. These
-- system tables describe them: their definitions, every schema change that was
-- applied, the indexes ForgeBase manages, and prior versions of records in
-- versioned collections. Only the data-core service writes them.

-- One row per collection. `definition` is the full collection JSON (fields,
-- options); the columns next to it are for lookups and integrity.
CREATE TABLE _forgebase_collections (
    id         TEXT    NOT NULL PRIMARY KEY CHECK (length(id) BETWEEN 1 AND 64),
    name       TEXT    NOT NULL UNIQUE COLLATE NOCASE CHECK (length(name) BETWEEN 1 AND 63),
    type       TEXT    NOT NULL CHECK (type IN ('base', 'auth', 'view')),
    system     INTEGER NOT NULL DEFAULT 0 CHECK (system IN (0, 1)),
    definition TEXT    NOT NULL CHECK (json_valid(definition)),
    version    INTEGER NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at TEXT    NOT NULL,
    updated_at TEXT    NOT NULL
) STRICT;

-- Append-only log of applied schema changes (collections API and SQL editor DDL).
-- Written in the same transaction as the change itself: a change that rolls
-- back leaves no history row.
CREATE TABLE _forgebase_schema_history (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    collection_id TEXT,
    collection    TEXT,
    version       INTEGER,
    action        TEXT NOT NULL CHECK (action IN ('create', 'alter', 'drop', 'sql', 'index_create', 'index_drop')),
    statements    TEXT NOT NULL CHECK (json_valid(statements)),
    before        TEXT CHECK (before IS NULL OR json_valid(before)),
    after         TEXT CHECK (after IS NULL OR json_valid(after)),
    actor         TEXT NOT NULL,
    applied_at    TEXT NOT NULL
) STRICT;

CREATE INDEX _forgebase_schema_history_by_collection ON _forgebase_schema_history (collection_id, id);

-- Indexes managed by ForgeBase (automatic from the schema, or created through
-- the index manager). Indexes created with raw SQL are inspected from the
-- engine catalog and reported with origin "sql".
CREATE TABLE _forgebase_indexes (
    name          TEXT NOT NULL PRIMARY KEY,
    collection_id TEXT NOT NULL REFERENCES _forgebase_collections (id) ON DELETE CASCADE,
    origin        TEXT NOT NULL CHECK (origin IN ('auto', 'manual')),
    kind          TEXT NOT NULL CHECK (kind IN ('btree', 'fulltext')),
    definition    TEXT NOT NULL CHECK (json_valid(definition)),
    created_at    TEXT NOT NULL
) STRICT;

CREATE INDEX _forgebase_indexes_by_collection ON _forgebase_indexes (collection_id);

-- Record versions of collections with versioning enabled: one row per
-- create/update (full record JSON) and a tombstone per delete.
CREATE TABLE _forgebase_record_versions (
    collection_id TEXT    NOT NULL,
    record_id     TEXT    NOT NULL,
    version       INTEGER NOT NULL CHECK (version >= 1),
    op            TEXT    NOT NULL CHECK (op IN ('create', 'update', 'delete')),
    data          TEXT    CHECK (data IS NULL OR json_valid(data)),
    changed_at    TEXT    NOT NULL,
    actor         TEXT    NOT NULL,
    PRIMARY KEY (collection_id, record_id, version)
) STRICT, WITHOUT ROWID;
