-- FB-153: named direct logins and the SQL editor's role
-- (docs/requirements/rls-coverage.md §3.5, G11).
--
-- Direct logins exist on PostgreSQL only: the claims of named logins, the
-- roles and the editor's column grants live in the PostgreSQL twin
-- (migrations/postgres/0042_db_logins.sql). SQLite gets the same table,
-- always empty, so both backends have the same system tables (the shard
-- placement registry, ADR-0029 §3.2, lists every one).
CREATE TABLE IF NOT EXISTS _forgebase_db_logins (
    name                 TEXT    NOT NULL PRIMARY KEY CHECK (name GLOB '[a-z]*' AND length(name) <= 40),
    role_name            TEXT    NOT NULL UNIQUE,
    principal            TEXT    NOT NULL,
    tenant               TEXT,
    readonly             INTEGER NOT NULL DEFAULT 0 CHECK (readonly IN (0, 1)),
    admin                INTEGER NOT NULL DEFAULT 0 CHECK (admin IN (0, 1)),
    connection_limit     INTEGER NOT NULL DEFAULT 10,
    statement_timeout_ms INTEGER NOT NULL DEFAULT 30000,
    idle_timeout_ms      INTEGER NOT NULL DEFAULT 60000,
    valid_until          TEXT,
    claims               TEXT    CHECK (claims IS NULL OR json_valid(claims)),
    claims_at            TEXT,
    created_by           TEXT    NOT NULL,
    created_at           TEXT    NOT NULL,
    rotated_at           TEXT
) STRICT;
