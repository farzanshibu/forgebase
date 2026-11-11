-- FB-152: native row-level security (docs/requirements/rls-coverage.md §3).
--
-- PostgreSQL only: roles, the `api` schema, `forgebase.claims()` and the
-- helper functions that generated policies call live in the PostgreSQL twin
-- (migrations/postgres/0040_native_rls.sql). SQLite has no roles or
-- policies; the data core keeps enforcing every rule itself there.
SELECT 1;
