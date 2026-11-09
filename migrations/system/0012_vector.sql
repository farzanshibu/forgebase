-- 0012_vector: vector collections (FB-50, SPEC §6, ADR-0012).
--
-- Canonical vectors and their metadata live here. The approximate
-- nearest-neighbour (ANN) indexes under <data_dir>/indexes/vector/ are derived
-- from these tables and are rebuilt from them when missing, corrupt, stale or
-- written by another engine version.
--
-- Tenancy: a collection belongs to one tenant ('' = the default tenant) and
-- every item, vector and index file hangs off the collection's integer id. The
-- service resolves (tenant, name) to that id first and never takes an id from
-- input, so a caller can only reach its own tenant's rows. Composite foreign
-- keys keep a vector inside the collection of its item.

CREATE TABLE _forgebase_vector_collections (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    tenant      TEXT    NOT NULL DEFAULT '',
    name        TEXT    NOT NULL CHECK (length(name) BETWEEN 1 AND 63),
    description TEXT    NOT NULL DEFAULT '' CHECK (length(description) <= 1024),
    options     TEXT    NOT NULL DEFAULT '{}' CHECK (json_valid(options) AND json_type(options) = 'object'),
    -- Incremented by every committed write; an index file records the value
    -- it reflects, so a file older than the data is detected as stale.
    change_seq  INTEGER NOT NULL DEFAULT 0,
    created_at  TEXT    NOT NULL,
    updated_at  TEXT    NOT NULL,
    UNIQUE (tenant, name)
) STRICT;

CREATE TABLE _forgebase_vector_fields (
    collection_id    INTEGER NOT NULL REFERENCES _forgebase_vector_collections (id) ON DELETE CASCADE,
    name             TEXT    NOT NULL CHECK (length(name) BETWEEN 1 AND 63),
    dimension        INTEGER NOT NULL CHECK (dimension BETWEEN 1 AND 4096),
    metric           TEXT    NOT NULL CHECK (metric IN ('cosine', 'l2', 'dot')),
    quantization     TEXT    NOT NULL DEFAULT 'f32' CHECK (quantization IN ('f32', 'f16', 'i8')),
    connectivity     INTEGER NOT NULL CHECK (connectivity BETWEEN 2 AND 128),
    expansion_add    INTEGER NOT NULL CHECK (expansion_add BETWEEN 8 AND 4096),
    expansion_search INTEGER NOT NULL CHECK (expansion_search BETWEEN 8 AND 4096),
    created_at       TEXT    NOT NULL,
    PRIMARY KEY (collection_id, name)
) STRICT, WITHOUT ROWID;

-- `key` is the ANN key. AUTOINCREMENT: a key is never reused, so an index
-- entry can never point at a different item than the one it was built from.
-- Column names follow the data core (`id`, `created`, `updated`, `_version`,
-- `_data`), so the filter language compiles against this table unchanged:
-- undeclared paths resolve inside the `_data` metadata document.
CREATE TABLE _forgebase_vector_items (
    key           INTEGER PRIMARY KEY AUTOINCREMENT,
    collection_id INTEGER NOT NULL REFERENCES _forgebase_vector_collections (id) ON DELETE CASCADE,
    id            TEXT    NOT NULL CHECK (length(id) BETWEEN 1 AND 64),
    _data         TEXT    NOT NULL DEFAULT '{}' CHECK (json_valid(_data) AND json_type(_data) = 'object'),
    content       TEXT,
    _version      INTEGER NOT NULL DEFAULT 1,
    created       TEXT    NOT NULL,
    updated       TEXT    NOT NULL,
    UNIQUE (collection_id, id),
    UNIQUE (collection_id, key)
) STRICT;

-- One row per (item, vector field). Little-endian IEEE 754 float32 values.
CREATE TABLE _forgebase_vector_values (
    collection_id INTEGER NOT NULL,
    field         TEXT    NOT NULL,
    key           INTEGER NOT NULL,
    vector        BLOB    NOT NULL,
    PRIMARY KEY (collection_id, field, key),
    FOREIGN KEY (collection_id, key) REFERENCES _forgebase_vector_items (collection_id, key) ON DELETE CASCADE,
    FOREIGN KEY (collection_id, field) REFERENCES _forgebase_vector_fields (collection_id, name) ON DELETE CASCADE
) STRICT;

CREATE INDEX _forgebase_vector_values_by_key ON _forgebase_vector_values (collection_id, key);

-- Keyword side of hybrid search: one contentless FTS5 table for all
-- collections, rowid = item key. Queries join back to the items of one
-- collection. Kept in sync by triggers, so no write path can forget it.
CREATE VIRTUAL TABLE _forgebase_vector_fts USING fts5(
    content,
    content = '',
    contentless_delete = 1,
    tokenize = 'unicode61 remove_diacritics 2'
);

CREATE TRIGGER _forgebase_vector_items_fts_insert AFTER INSERT ON _forgebase_vector_items
WHEN new.content IS NOT NULL
BEGIN
    INSERT INTO _forgebase_vector_fts (rowid, content) VALUES (new.key, new.content);
END;

CREATE TRIGGER _forgebase_vector_items_fts_delete AFTER DELETE ON _forgebase_vector_items
WHEN old.content IS NOT NULL
BEGIN
    DELETE FROM _forgebase_vector_fts WHERE rowid = old.key;
END;

CREATE TRIGGER _forgebase_vector_items_fts_update AFTER UPDATE OF content ON _forgebase_vector_items
BEGIN
    DELETE FROM _forgebase_vector_fts WHERE rowid = old.key AND old.content IS NOT NULL;
    INSERT INTO _forgebase_vector_fts (rowid, content) SELECT new.key, new.content WHERE new.content IS NOT NULL;
END;
