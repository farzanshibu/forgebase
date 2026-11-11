-- Managed replicas on other hosts (FB-168, ADR-0029 §2.3): the agent of a
-- `forgebase node join` and the commands the server queues for it.
--
-- _forgebase_db_node_agents: one row per remote node that redeemed a join
-- token. `key_hash` is the SHA-256 (hex) of the agent's key, shown to the
-- agent once; the last heartbeat (state, replay position, free disk, engine
-- version) is kept here, node health transitions go to _forgebase_db_nodes.
-- _forgebase_db_node_commands: rebuild, stop, promote, rejoin and follow,
-- delivered by the agent's outbound long-poll and answered in a heartbeat.

CREATE TABLE _forgebase_db_node_agents (
    node_id        TEXT    NOT NULL PRIMARY KEY REFERENCES _forgebase_db_nodes(id) ON DELETE CASCADE,
    key_hash       TEXT    NOT NULL UNIQUE CHECK (length(key_hash) = 64),
    address        TEXT,
    engine_version TEXT    NOT NULL,
    state          TEXT    NOT NULL DEFAULT 'joining'
                   CHECK (state IN ('joining', 'cloning', 'starting', 'catching_up', 'streaming', 'stopped',
                                    'rebuilding', 'promoting', 'primary', 'failed')),
    replay_lsn     TEXT,
    disk_free      INTEGER CHECK (disk_free >= 0),
    detail         TEXT,
    last_seen_at   TEXT,
    created_at     TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at     TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
) STRICT;

CREATE TABLE _forgebase_db_node_commands (
    id         TEXT NOT NULL PRIMARY KEY CHECK (length(id) BETWEEN 1 AND 64),
    node_id    TEXT NOT NULL REFERENCES _forgebase_db_nodes(id) ON DELETE CASCADE,
    kind       TEXT NOT NULL CHECK (kind IN ('rebuild', 'stop', 'promote', 'rejoin', 'follow')),
    args       TEXT NOT NULL DEFAULT '{}',
    state      TEXT NOT NULL DEFAULT 'queued' CHECK (state IN ('queued', 'sent', 'done', 'failed', 'cancelled')),
    result     TEXT,
    created_by TEXT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    sent_at    TEXT,
    done_at    TEXT
) STRICT;
CREATE INDEX _forgebase_db_node_commands_open ON _forgebase_db_node_commands (node_id, state, created_at);
