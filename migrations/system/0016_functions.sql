-- 0016_functions: server functions (FB-60, ADR-0017).
--
-- A function has numbered, immutable versions; `active_version` names the one
-- that runs, so a rollback is a single UPDATE. A version row holds the module
-- (WebAssembly binary) and its configuration: granted capabilities, allowed
-- fetch hosts, limits, plain environment values and record hooks, so a
-- rollback restores all of them together. Secrets are per function (not per
-- version) and stored sealed (XChaCha20-Poly1305 under a key derived from
-- the data directory's master key, bound to function id and name).

CREATE TABLE _forgebase_functions (
    id             TEXT PRIMARY KEY,
    name           TEXT NOT NULL UNIQUE,
    description    TEXT NOT NULL DEFAULT '',
    access         TEXT NOT NULL CHECK (access IN ('public', 'authenticated', 'permission')),
    active_version INTEGER NOT NULL CHECK (active_version >= 1),
    latest_version INTEGER NOT NULL CHECK (latest_version >= active_version),
    created_at     TEXT NOT NULL,
    updated_at     TEXT NOT NULL,
    created_by     TEXT NOT NULL
) STRICT;

CREATE TABLE _forgebase_function_versions (
    function_id TEXT NOT NULL REFERENCES _forgebase_functions (id) ON DELETE CASCADE,
    version     INTEGER NOT NULL CHECK (version >= 1),
    wasm        BLOB NOT NULL,
    sha256      TEXT NOT NULL,
    config      TEXT NOT NULL CHECK (json_valid(config)),
    created_at  TEXT NOT NULL,
    created_by  TEXT NOT NULL,
    PRIMARY KEY (function_id, version)
) STRICT;

CREATE TABLE _forgebase_function_secrets (
    function_id TEXT NOT NULL REFERENCES _forgebase_functions (id) ON DELETE CASCADE,
    name        TEXT NOT NULL,
    sealed      BLOB NOT NULL,
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL,
    PRIMARY KEY (function_id, name)
) STRICT;

-- Execution log: one row per execution, newest rows kept per function.
CREATE TABLE _forgebase_function_logs (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    function_id TEXT NOT NULL REFERENCES _forgebase_functions (id) ON DELETE CASCADE,
    function    TEXT NOT NULL,
    version     INTEGER NOT NULL,
    trigger     TEXT NOT NULL,
    status      TEXT NOT NULL,
    duration_ms INTEGER NOT NULL,
    fuel_used   INTEGER NOT NULL,
    error       TEXT,
    logs        TEXT NOT NULL CHECK (json_valid(logs)),
    request_id  TEXT,
    created_at  TEXT NOT NULL
) STRICT;

CREATE INDEX _forgebase_function_logs_function ON _forgebase_function_logs (function_id, id);

-- Consumer cursors over the realtime outbox (after-hooks).
CREATE TABLE _forgebase_function_cursors (
    name       TEXT PRIMARY KEY,
    seq        INTEGER NOT NULL,
    updated_at TEXT NOT NULL
) STRICT;
