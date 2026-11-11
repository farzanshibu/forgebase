-- 0033_db_topology: the database topology store (FB-170, ADR-0029 §3.4, §4.2).
--
-- Control tables: they hold rows on the control set only (placement class
-- `control`, `forgebase_db::placement::TABLES`), although every node has the
-- full schema. A single-server deployment has exactly one set, `control`
-- (number 0), seeded below; it hosts every tenant, and a tenant without a
-- placement row lives on control. SQLite projects never get more than this
-- one set (replicas and shards need PostgreSQL).
--
-- Times are RFC 3339 text (UTC, milliseconds), as elsewhere in the schema.

-- One row per replica set: control (number 0) or a shard (1 ... 8190).
-- Identity sequences on set k draw from [k * 2^40 + 1, (k + 1) * 2^40 - 1]
-- (ADR-0029 §3.3), so `number` never changes and is never reused while
-- rows allocated from its range exist.
CREATE TABLE _forgebase_db_sets (
    id            TEXT    NOT NULL PRIMARY KEY CHECK (length(id) BETWEEN 1 AND 64),
    name          TEXT    NOT NULL UNIQUE COLLATE NOCASE CHECK (length(name) BETWEEN 1 AND 63),
    kind          TEXT    NOT NULL CHECK (kind IN ('control', 'shard')),
    number        INTEGER NOT NULL UNIQUE CHECK (number BETWEEN 0 AND 8190),
    -- provisioning: being added, not routable; active: takes new tenants;
    -- draining: no new tenants, moves out; migrating: below the binary's
    -- schema head (its tenants get 503 shard_migrating); retired: removed.
    state         TEXT    NOT NULL DEFAULT 'active'
                  CHECK (state IN ('provisioning', 'active', 'draining', 'migrating', 'retired')),
    -- Relative capacity for placement: load score = estimated bytes / weight.
    weight        INTEGER NOT NULL DEFAULT 1 CHECK (weight BETWEEN 1 AND 10000),
    -- 0: the set takes no new tenants (control after it was drained).
    hosts_tenants INTEGER NOT NULL DEFAULT 1 CHECK (hosts_tenants IN (0, 1)),
    created_at    TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at    TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    -- Exactly the control set has number 0 (number is unique).
    CHECK ((kind = 'control') = (number = 0))
) STRICT;

-- One row per PostgreSQL server of a set. Control's primary may have no row:
-- it is the database the server was started with (bundled engine or the
-- configured URL).
CREATE TABLE _forgebase_db_nodes (
    id                TEXT    NOT NULL PRIMARY KEY CHECK (length(id) BETWEEN 1 AND 64),
    set_id            TEXT    NOT NULL REFERENCES _forgebase_db_sets(id),
    name              TEXT    NOT NULL UNIQUE COLLATE NOCASE CHECK (length(name) BETWEEN 1 AND 63),
    -- local: bundled engine on this host (postgres/nodes/<name>); remote:
    -- bundled engine on another host (`forgebase node`); external: a URL.
    kind              TEXT    NOT NULL CHECK (kind IN ('local', 'remote', 'external')),
    role              TEXT    NOT NULL CHECK (role IN ('primary', 'replica')),
    -- Connection URL sealed with the instance keyring; NULL for local nodes.
    url_sealed        TEXT,
    state             TEXT    NOT NULL DEFAULT 'provisioning'
                      CHECK (state IN ('provisioning', 'ready', 'catching_up', 'streaming', 'lagging',
                                       'unreachable', 'needs_rebuild', 'promoting', 'retired')),
    -- Share of replica reads (0: out of rotation).
    weight            INTEGER NOT NULL DEFAULT 1 CHECK (weight BETWEEN 0 AND 10000),
    engine_version    TEXT,
    -- pg_control_system().system_identifier, as text (an unsigned 64-bit value).
    system_identifier TEXT,
    timeline          INTEGER,
    -- api: added through /api/system/database; config: [[database.nodes]]
    -- in forgebase.toml (read-only in the dashboard).
    origin            TEXT    NOT NULL DEFAULT 'api' CHECK (origin IN ('api', 'config')),
    created_at        TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at        TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
) STRICT;
CREATE INDEX _forgebase_db_nodes_set ON _forgebase_db_nodes (set_id, role);
CREATE UNIQUE INDEX _forgebase_db_nodes_one_primary ON _forgebase_db_nodes (set_id)
    WHERE role = 'primary' AND state <> 'retired';

-- Where each tenant (environment id) lives. No row: control. `epoch` grows
-- by one on every change of the row, so a cached placement can tell it is
-- stale. No foreign key to the environment: the row outlives a deleted
-- environment until its data on the shard is gone.
CREATE TABLE _forgebase_tenant_placements (
    tenant     TEXT    NOT NULL PRIMARY KEY CHECK (length(tenant) BETWEEN 1 AND 64),
    set_id     TEXT    NOT NULL REFERENCES _forgebase_db_sets(id),
    epoch      INTEGER NOT NULL DEFAULT 1 CHECK (epoch >= 1),
    -- active; moving: being copied (still served by set_id); frozen: the
    -- move's switch (requests wait); cleanup: moved, source rows not yet deleted.
    state      TEXT    NOT NULL DEFAULT 'active' CHECK (state IN ('active', 'moving', 'frozen', 'cleanup')),
    moved_at   TEXT,
    -- Size estimates refreshed by the statistics job (load score, plans).
    est_rows   INTEGER NOT NULL DEFAULT 0 CHECK (est_rows >= 0),
    est_bytes  INTEGER NOT NULL DEFAULT 0 CHECK (est_bytes >= 0),
    stats_at   TEXT,
    created_at TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
) STRICT;
CREATE INDEX _forgebase_tenant_placements_set ON _forgebase_tenant_placements (set_id, tenant);

-- Online tenant moves (job database.tenant.move, ADR-0029 §3.8). `detail`
-- is the move's checkpoint (publication, slot, tables, counts).
CREATE TABLE _forgebase_db_moves (
    id           TEXT NOT NULL PRIMARY KEY CHECK (length(id) BETWEEN 1 AND 64),
    tenant       TEXT NOT NULL CHECK (length(tenant) BETWEEN 1 AND 64),
    from_set     TEXT NOT NULL REFERENCES _forgebase_db_sets(id),
    to_set       TEXT NOT NULL REFERENCES _forgebase_db_sets(id),
    state        TEXT NOT NULL DEFAULT 'preparing'
                 CHECK (state IN ('preparing', 'copying', 'catching_up', 'switching', 'cleanup', 'done', 'aborted')),
    job_id       TEXT,
    rebalance_id TEXT,
    detail       TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(detail)),
    error        TEXT,
    created_by   TEXT,
    created_at   TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at   TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    switched_at  TEXT,
    finished_at  TEXT,
    CHECK (from_set <> to_set)
) STRICT;
CREATE INDEX _forgebase_db_moves_tenant ON _forgebase_db_moves (tenant, created_at);
CREATE UNIQUE INDEX _forgebase_db_moves_one_open ON _forgebase_db_moves (tenant)
    WHERE state NOT IN ('done', 'aborted');

-- Two-phase schema changes across sets (ADR-0029 §3.7). A row is written,
-- in control, only once every node has prepared `<gid>_<set number>`; an
-- in-doubt prepared transaction with no row is rolled back, one with a
-- `committing` row is committed. `nodes` is the JSON array of set ids.
CREATE TABLE _forgebase_xact_journal (
    gid         TEXT NOT NULL PRIMARY KEY CHECK (length(gid) BETWEEN 1 AND 180),
    nodes       TEXT NOT NULL CHECK (json_valid(nodes)),
    state       TEXT NOT NULL DEFAULT 'committing' CHECK (state IN ('committing', 'committed')),
    purpose     TEXT NOT NULL DEFAULT '',
    created_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    resolved_at TEXT
) STRICT;
CREATE INDEX _forgebase_xact_journal_open ON _forgebase_xact_journal (created_at) WHERE state = 'committing';

-- One-time tokens for `forgebase node join` (ADR-0029 §2.3). Only the
-- SHA-256 of the token is stored; the token is shown once.
CREATE TABLE _forgebase_join_tokens (
    id         TEXT NOT NULL PRIMARY KEY CHECK (length(id) BETWEEN 1 AND 64),
    token_hash TEXT NOT NULL UNIQUE CHECK (length(token_hash) = 64),
    set_id     TEXT NOT NULL REFERENCES _forgebase_db_sets(id) ON DELETE CASCADE,
    role       TEXT NOT NULL CHECK (role IN ('primary', 'replica')),
    created_by TEXT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    expires_at TEXT NOT NULL,
    used_at    TEXT,
    node_id    TEXT REFERENCES _forgebase_db_nodes(id) ON DELETE SET NULL
) STRICT;
CREATE INDEX _forgebase_join_tokens_set ON _forgebase_join_tokens (set_id);

INSERT INTO _forgebase_db_sets (id, name, kind, number, state, weight, hosts_tenants)
VALUES ('control', 'control', 'control', 0, 'active', 1, 1);
