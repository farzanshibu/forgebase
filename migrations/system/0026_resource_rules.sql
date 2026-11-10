-- FB-134 (ADR-0028 addendum "FB-134"): rules of named engine resources (a KV
-- namespace, vector collection, graph label or edge type, bucket, queue topic
-- or subscription). One row per (kind, name, action); `name` '*' holds the
-- rules of every resource of the kind that has no rules of its own. `action`
-- is list, view, create, update, delete, read (list + view) or write (create +
-- update + delete); `rule` NULL locks the action, '' opens it to everyone.
-- A rule only widens access for callers without the engine permission
-- (`engine:<action>` or `engine:<action>:<name>`); it never grants admin
-- actions.
CREATE TABLE _forgebase_resource_rules (
    kind       TEXT    NOT NULL,
    name       TEXT    NOT NULL,
    action     TEXT    NOT NULL,
    rule       TEXT,
    updated_by TEXT    NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (kind, name, action)
) STRICT;
