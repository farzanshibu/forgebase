-- PostgreSQL twin of migrations/system/0033_db_topology.sql (FB-170,
-- ADR-0029 §3.4, §4.2): the database topology store. Control tables
-- (placement class `control`); the control set (number 0) is seeded and
-- hosts every tenant without a placement row. Comments on each column are
-- in the SQLite file.

CREATE TABLE _forgebase_db_sets (
    id            text     NOT NULL PRIMARY KEY CHECK (length(id) BETWEEN 1 AND 64),
    name          citext   NOT NULL UNIQUE CHECK (length(name) BETWEEN 1 AND 63),
    kind          text     NOT NULL CHECK (kind IN ('control', 'shard')),
    number        bigint   NOT NULL UNIQUE CHECK (number BETWEEN 0 AND 8190),
    state         text     NOT NULL DEFAULT 'active'
                  CHECK (state IN ('provisioning', 'active', 'draining', 'migrating', 'retired')),
    weight        bigint   NOT NULL DEFAULT 1 CHECK (weight BETWEEN 1 AND 10000),
    hosts_tenants smallint NOT NULL DEFAULT 1 CHECK (hosts_tenants IN (0, 1)),
    created_at    text     NOT NULL DEFAULT to_char(now() AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"'),
    updated_at    text     NOT NULL DEFAULT to_char(now() AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"'),
    CHECK ((kind = 'control') = (number = 0))
);

CREATE TABLE _forgebase_db_nodes (
    id                text   NOT NULL PRIMARY KEY CHECK (length(id) BETWEEN 1 AND 64),
    set_id            text   NOT NULL REFERENCES _forgebase_db_sets(id) DEFERRABLE INITIALLY IMMEDIATE,
    name              citext NOT NULL UNIQUE CHECK (length(name) BETWEEN 1 AND 63),
    kind              text   NOT NULL CHECK (kind IN ('local', 'remote', 'external')),
    role              text   NOT NULL CHECK (role IN ('primary', 'replica')),
    url_sealed        text,
    state             text   NOT NULL DEFAULT 'provisioning'
                      CHECK (state IN ('provisioning', 'ready', 'catching_up', 'streaming', 'lagging',
                                       'unreachable', 'needs_rebuild', 'promoting', 'retired')),
    weight            bigint NOT NULL DEFAULT 1 CHECK (weight BETWEEN 0 AND 10000),
    engine_version    text,
    system_identifier text,
    timeline          bigint,
    origin            text   NOT NULL DEFAULT 'api' CHECK (origin IN ('api', 'config')),
    created_at        text   NOT NULL DEFAULT to_char(now() AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"'),
    updated_at        text   NOT NULL DEFAULT to_char(now() AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"')
);
CREATE INDEX _forgebase_db_nodes_set ON _forgebase_db_nodes (set_id, role);
CREATE UNIQUE INDEX _forgebase_db_nodes_one_primary ON _forgebase_db_nodes (set_id)
    WHERE role = 'primary' AND state <> 'retired';

CREATE TABLE _forgebase_tenant_placements (
    tenant     text   NOT NULL PRIMARY KEY CHECK (length(tenant) BETWEEN 1 AND 64),
    set_id     text   NOT NULL REFERENCES _forgebase_db_sets(id) DEFERRABLE INITIALLY IMMEDIATE,
    epoch      bigint NOT NULL DEFAULT 1 CHECK (epoch >= 1),
    state      text   NOT NULL DEFAULT 'active' CHECK (state IN ('active', 'moving', 'frozen', 'cleanup')),
    moved_at   text,
    est_rows   bigint NOT NULL DEFAULT 0 CHECK (est_rows >= 0),
    est_bytes  bigint NOT NULL DEFAULT 0 CHECK (est_bytes >= 0),
    stats_at   text,
    created_at text   NOT NULL DEFAULT to_char(now() AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"'),
    updated_at text   NOT NULL DEFAULT to_char(now() AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"')
);
CREATE INDEX _forgebase_tenant_placements_set ON _forgebase_tenant_placements (set_id, tenant);

CREATE TABLE _forgebase_db_moves (
    id           text  NOT NULL PRIMARY KEY CHECK (length(id) BETWEEN 1 AND 64),
    tenant       text  NOT NULL CHECK (length(tenant) BETWEEN 1 AND 64),
    from_set     text  NOT NULL REFERENCES _forgebase_db_sets(id) DEFERRABLE INITIALLY IMMEDIATE,
    to_set       text  NOT NULL REFERENCES _forgebase_db_sets(id) DEFERRABLE INITIALLY IMMEDIATE,
    state        text  NOT NULL DEFAULT 'preparing'
                 CHECK (state IN ('preparing', 'copying', 'catching_up', 'switching', 'cleanup', 'done', 'aborted')),
    job_id       text,
    rebalance_id text,
    detail       jsonb NOT NULL DEFAULT '{}',
    error        text,
    created_by   text,
    created_at   text  NOT NULL DEFAULT to_char(now() AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"'),
    updated_at   text  NOT NULL DEFAULT to_char(now() AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"'),
    switched_at  text,
    finished_at  text,
    CHECK (from_set <> to_set)
);
CREATE INDEX _forgebase_db_moves_tenant ON _forgebase_db_moves (tenant, created_at);
CREATE UNIQUE INDEX _forgebase_db_moves_one_open ON _forgebase_db_moves (tenant)
    WHERE state NOT IN ('done', 'aborted');

CREATE TABLE _forgebase_xact_journal (
    gid         text  NOT NULL PRIMARY KEY CHECK (length(gid) BETWEEN 1 AND 180),
    nodes       jsonb NOT NULL,
    state       text  NOT NULL DEFAULT 'committing' CHECK (state IN ('committing', 'committed')),
    purpose     text  NOT NULL DEFAULT '',
    created_at  text  NOT NULL DEFAULT to_char(now() AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"'),
    resolved_at text
);
CREATE INDEX _forgebase_xact_journal_open ON _forgebase_xact_journal (created_at) WHERE state = 'committing';

CREATE TABLE _forgebase_join_tokens (
    id         text NOT NULL PRIMARY KEY CHECK (length(id) BETWEEN 1 AND 64),
    token_hash text NOT NULL UNIQUE CHECK (length(token_hash) = 64),
    set_id     text NOT NULL REFERENCES _forgebase_db_sets(id) ON DELETE CASCADE DEFERRABLE INITIALLY IMMEDIATE,
    role       text NOT NULL CHECK (role IN ('primary', 'replica')),
    created_by text,
    created_at text NOT NULL DEFAULT to_char(now() AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"'),
    expires_at text NOT NULL,
    used_at    text,
    node_id    text REFERENCES _forgebase_db_nodes(id) ON DELETE SET NULL DEFERRABLE INITIALLY IMMEDIATE
);
CREATE INDEX _forgebase_join_tokens_set ON _forgebase_join_tokens (set_id);

INSERT INTO _forgebase_db_sets (id, name, kind, number, state, weight, hosts_tenants)
VALUES ('control', 'control', 'control', 0, 'active', 1, 1);
