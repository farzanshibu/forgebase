-- PostgreSQL twin of migrations/system/0027_functions_js.sql (FB-93,
-- ADR-0027): JavaScript and TypeScript function versions keep their source,
-- the JavaScript the engine runs and, for TypeScript, a source map (JSON
-- text); `_forgebase_function_kv_rules` lets a function reach application
-- key-value namespaces (deny by default).

ALTER TABLE _forgebase_function_versions ADD COLUMN source text;
ALTER TABLE _forgebase_function_versions ADD COLUMN js text;
ALTER TABLE _forgebase_function_versions ADD COLUMN source_map text;

CREATE TABLE _forgebase_function_kv_rules (
    function_id text NOT NULL REFERENCES _forgebase_functions (id) ON DELETE CASCADE DEFERRABLE INITIALLY IMMEDIATE,
    namespace   text NOT NULL,
    access      text NOT NULL CHECK (access IN ('read', 'write')),
    created_at  text NOT NULL,
    created_by  text NOT NULL,
    PRIMARY KEY (function_id, namespace)
);
