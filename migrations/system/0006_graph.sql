-- 0006_graph: graph model on SQLite (FB-51, SPEC §7).
--
-- Nodes and typed edges live in two indexed relational tables. Every row
-- carries the tenant it belongs to ('' = the default tenant); every query the
-- graph service runs filters on it, and an edge can only join two nodes of its
-- own tenant (composite foreign keys). Both traversal directions are indexed:
-- (tenant, source, type, target) for outgoing and (tenant, target, type,
-- source) for incoming edges. An undirected edge (directed = 0) is stored once
-- and walked both ways by the service.

CREATE TABLE _forgebase_graph_nodes (
    tenant     TEXT NOT NULL DEFAULT '',
    id         TEXT NOT NULL CHECK (length(id) BETWEEN 1 AND 128),
    label      TEXT NOT NULL DEFAULT '' CHECK (length(label) <= 64),
    properties TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(properties) AND json_type(properties) = 'object'),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (tenant, id)
) STRICT, WITHOUT ROWID;

CREATE INDEX _forgebase_graph_nodes_by_label ON _forgebase_graph_nodes (tenant, label, id);

CREATE TABLE _forgebase_graph_edges (
    tenant     TEXT    NOT NULL DEFAULT '',
    id         TEXT    NOT NULL CHECK (length(id) BETWEEN 1 AND 128),
    type       TEXT    NOT NULL CHECK (length(type) BETWEEN 1 AND 64),
    source     TEXT    NOT NULL,
    target     TEXT    NOT NULL,
    directed   INTEGER NOT NULL DEFAULT 1 CHECK (directed IN (0, 1)),
    properties TEXT    NOT NULL DEFAULT '{}' CHECK (json_valid(properties) AND json_type(properties) = 'object'),
    created_at TEXT    NOT NULL,
    updated_at TEXT    NOT NULL,
    PRIMARY KEY (tenant, id),
    FOREIGN KEY (tenant, source) REFERENCES _forgebase_graph_nodes (tenant, id) ON DELETE CASCADE,
    FOREIGN KEY (tenant, target) REFERENCES _forgebase_graph_nodes (tenant, id) ON DELETE CASCADE
) STRICT, WITHOUT ROWID;

CREATE INDEX _forgebase_graph_edges_out ON _forgebase_graph_edges (tenant, source, type, target);
CREATE INDEX _forgebase_graph_edges_in  ON _forgebase_graph_edges (tenant, target, type, source);
CREATE INDEX _forgebase_graph_edges_by_type ON _forgebase_graph_edges (tenant, type, id);
