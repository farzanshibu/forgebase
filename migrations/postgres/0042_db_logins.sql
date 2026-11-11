-- PostgreSQL twin of migrations/system/0042_db_logins.sql (FB-153): named
-- direct logins, the claims they get, and the SQL editor's role
-- (docs/requirements/rls-coverage.md §3.2, §3.5, G11).
--
-- Roles and memberships are made by the bundled engine's bootstrap
-- (FB-132/FB-153), which runs as forgebase_super. This migration runs as
-- forgebase_owner there and cannot create roles; on clusters where the
-- migrating role is a superuser (test clusters) the missing ones are made
-- here.
--
-- * forgebase_sql_admin: the SQL editor and `--admin` logins. BYPASSRLS,
--   DML on `public`, CREATE on `public`, SELECT on the `forgebase` tables
--   except their secret columns (forgebase.is_secret_column), no grants on
--   secret columns of collection tables (forgebase-policy generates those
--   per collection).
-- * forgebase_login: every named login (`fbu_<name>`); SELECT on the `api`
--   views. forgebase_login_write: also INSERT, UPDATE, DELETE on them
--   (logins made without --readonly).

DO $fb$
DECLARE
  su boolean;
BEGIN
  SELECT r.rolsuper INTO su FROM pg_catalog.pg_roles r WHERE r.rolname = current_user;
  IF coalesce(su, false) THEN
    IF NOT EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = 'forgebase_sql_admin') THEN
      CREATE ROLE forgebase_sql_admin NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION BYPASSRLS;
    END IF;
    IF NOT EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = 'forgebase_login_write') THEN
      CREATE ROLE forgebase_login_write NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS;
    END IF;
    IF EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = 'forgebase_login') THEN
      GRANT forgebase_login TO forgebase_login_write;
    END IF;
  END IF;
END
$fb$;

-- One row per named login. `claims` is what forgebase.claims() returns for
-- that login's sessions; ForgeBase rewrites it after every policy snapshot
-- reload and every collection change. The password is never stored here
-- (PostgreSQL keeps the SCRAM verifier).
CREATE TABLE IF NOT EXISTS _forgebase_db_logins (
    name               text    NOT NULL PRIMARY KEY CHECK (name ~ '^[a-z][a-z0-9_]{0,39}$'),
    role_name          text    NOT NULL UNIQUE,
    principal          text    NOT NULL,
    tenant             text,
    readonly           smallint NOT NULL DEFAULT 0 CHECK (readonly IN (0, 1)),
    admin              smallint NOT NULL DEFAULT 0 CHECK (admin IN (0, 1)),
    connection_limit   bigint  NOT NULL DEFAULT 10,
    statement_timeout_ms bigint NOT NULL DEFAULT 30000,
    idle_timeout_ms    bigint  NOT NULL DEFAULT 60000,
    valid_until        text,
    claims             jsonb,
    claims_at          text,
    created_by         text    NOT NULL,
    created_at         text    NOT NULL,
    rotated_at         text
);
ALTER TABLE _forgebase_db_logins ENABLE ROW LEVEL SECURITY;

-- Which caller the policies see (FB-152 + FB-153). Sessions whose login
-- role bypasses row-level security (the server, the reader, admin roles)
-- set the claims themselves; a named login gets the claims ForgeBase
-- stored for it; anything else gets NULL, so every policy fails.
CREATE OR REPLACE FUNCTION forgebase.claims() RETURNS jsonb
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = forgebase, pg_catalog AS $$
  SELECT CASE
    WHEN (SELECT r.rolsuper OR r.rolbypassrls FROM pg_catalog.pg_roles r WHERE r.rolname = session_user)
      THEN nullif(current_setting('forgebase.claims', true), '')::jsonb
    ELSE (SELECT l.claims FROM forgebase._forgebase_db_logins l WHERE l.role_name = session_user)
  END
$$;

-- Columns the SQL editor's role never reads (G11): password hashes,
-- tokens, sealed and encrypted secrets, and documents that copy records
-- with their password hashes (record versions, the realtime outbox).
CREATE OR REPLACE FUNCTION forgebase.is_secret_column(tbl text, col text) RETURNS boolean
LANGUAGE sql IMMUTABLE SET search_path = forgebase, pg_catalog AS $$
  SELECT (tbl, col) IN (('_forgebase_storage_settings', 'value'), ('_forgebase_record_versions', 'data'),
                        ('_forgebase_realtime_outbox', 'snapshot'), ('_forgebase_db_logins', 'claims'))
      OR (col ~ '(^|_)(password|secret|token|sealed)(_|$)' AND col !~ '_(at|ttl)$')
      OR col ~ '_(hash|enc)$'
      OR col ~ '^private_'
$$;

-- (Re)grants the editor's role SELECT on every column of the `forgebase`
-- tables the caller owns, except the secret ones. Run by this migration and
-- by the server at every start (tables of later migrations).
CREATE OR REPLACE FUNCTION forgebase.fb_editor_grants() RETURNS void
LANGUAGE plpgsql SET search_path = forgebase, pg_catalog AS $$
DECLARE
  t record;
  cols text;
BEGIN
  IF NOT EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = 'forgebase_sql_admin') THEN
    RETURN;
  END IF;
  FOR t IN
    SELECT c.oid, c.relname
    FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
    WHERE n.nspname = 'forgebase' AND c.relkind IN ('r', 'p', 'v')
      AND pg_catalog.pg_has_role(current_user, c.relowner, 'USAGE')
  LOOP
    EXECUTE format('REVOKE ALL ON forgebase.%I FROM forgebase_sql_admin', t.relname);
    SELECT string_agg(format('%I', a.attname), ', ' ORDER BY a.attnum) INTO cols
    FROM pg_catalog.pg_attribute a
    WHERE a.attrelid = t.oid AND a.attnum > 0 AND NOT a.attisdropped
      AND NOT forgebase.is_secret_column(t.relname::text, a.attname::text);
    IF cols IS NOT NULL THEN
      EXECUTE format('GRANT SELECT (%s) ON forgebase.%I TO forgebase_sql_admin', cols, t.relname);
    END IF;
  END LOOP;
END
$$;
REVOKE ALL ON FUNCTION forgebase.fb_editor_grants() FROM PUBLIC;

DO $fb$
BEGIN
  IF EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = 'forgebase_sql_admin') THEN
    GRANT USAGE ON SCHEMA forgebase TO forgebase_sql_admin;
    GRANT USAGE, CREATE ON SCHEMA public TO forgebase_sql_admin;
    PERFORM forgebase.fb_editor_grants();
  END IF;
  IF EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = 'forgebase_login') THEN
    EXECUTE format('GRANT CONNECT ON DATABASE %I TO forgebase_login', current_database());
  END IF;
END
$fb$;
