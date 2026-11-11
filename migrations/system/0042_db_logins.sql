-- FB-153: named direct logins and the SQL editor's role
-- (docs/requirements/rls-coverage.md §3.5, G11).
--
-- PostgreSQL only: the `_forgebase_db_logins` table, the claims of named
-- logins and the editor's column grants live in the PostgreSQL twin
-- (migrations/postgres/0042_db_logins.sql). SQLite has no logins or roles.
SELECT 1;
