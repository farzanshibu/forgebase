-- 0026_functions_js: JavaScript and TypeScript functions (FB-93, ADR-0027).
--
-- A JS/TS version keeps the source as written (`source`) and the JavaScript
-- the embedded engine runs (`js`, the source itself for JavaScript, the
-- type-stripped output for TypeScript); its `wasm` is empty. The language and
-- the runtime are part of the version's configuration JSON (`language`,
-- `runtime`; absent = a WebAssembly version deployed before 0026).
--
-- `_forgebase_function_kv_rules` lets a function reach application key-value
-- namespaces (beyond its private `fn.<name>`): deny by default, one row per
-- (function, namespace pattern), managed by administrators. The caller's own
-- key-value permissions still apply on top.

ALTER TABLE _forgebase_function_versions ADD COLUMN source TEXT;
ALTER TABLE _forgebase_function_versions ADD COLUMN js TEXT;

CREATE TABLE _forgebase_function_kv_rules (
    function_id TEXT NOT NULL REFERENCES _forgebase_functions (id) ON DELETE CASCADE,
    namespace   TEXT NOT NULL,
    access      TEXT NOT NULL CHECK (access IN ('read', 'write')),
    created_at  TEXT NOT NULL,
    created_by  TEXT NOT NULL,
    PRIMARY KEY (function_id, namespace)
) STRICT;
