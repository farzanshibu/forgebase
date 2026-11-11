-- PostgreSQL twin of migrations/system/0043_db_node_agents.sql (FB-168,
-- ADR-0029 §2.3): agents of `forgebase node join` and their commands.

CREATE TABLE _forgebase_db_node_agents (
    node_id        text   NOT NULL PRIMARY KEY REFERENCES _forgebase_db_nodes(id) ON DELETE CASCADE,
    key_hash       text   NOT NULL UNIQUE CHECK (length(key_hash) = 64),
    address        text,
    engine_version text   NOT NULL,
    state          text   NOT NULL DEFAULT 'joining'
                   CHECK (state IN ('joining', 'cloning', 'starting', 'catching_up', 'streaming', 'stopped',
                                    'rebuilding', 'promoting', 'primary', 'failed')),
    replay_lsn     text,
    disk_free      bigint CHECK (disk_free >= 0),
    detail         text,
    last_seen_at   text,
    created_at     text   NOT NULL DEFAULT to_char(now() AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"'),
    updated_at     text   NOT NULL DEFAULT to_char(now() AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"')
);

CREATE TABLE _forgebase_db_node_commands (
    id         text NOT NULL PRIMARY KEY CHECK (length(id) BETWEEN 1 AND 64),
    node_id    text NOT NULL REFERENCES _forgebase_db_nodes(id) ON DELETE CASCADE,
    kind       text NOT NULL CHECK (kind IN ('rebuild', 'stop', 'promote', 'rejoin', 'follow')),
    args       text NOT NULL DEFAULT '{}',
    state      text NOT NULL DEFAULT 'queued' CHECK (state IN ('queued', 'sent', 'done', 'failed', 'cancelled')),
    result     text,
    created_by text,
    created_at text NOT NULL DEFAULT to_char(now() AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"'),
    sent_at    text,
    done_at    text
);
CREATE INDEX _forgebase_db_node_commands_open ON _forgebase_db_node_commands (node_id, state, created_at);
