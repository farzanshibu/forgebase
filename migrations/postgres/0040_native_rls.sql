-- PostgreSQL twin of migrations/system/0040_native_rls.sql (FB-152):
-- roles and schema privileges for native row-level security, the `api`
-- schema for direct logins, and the functions every generated policy calls
-- (docs/requirements/rls-coverage.md §3.2-§3.4). The policies themselves
-- are generated per collection by forgebase-policy (`native`) inside the
-- data core's schema transaction.
--
-- Roles are created by the bundled engine's bootstrap (FB-132), which runs
-- as forgebase_super. This migration runs as forgebase_owner there and
-- cannot create roles; on clusters where the migrating role may (a
-- superuser URL, test clusters) the missing ones are created here.
-- `forgebase_login` is the NOLOGIN group of named direct logins (FB-153):
-- generated `api` views are granted to it.

DO $fb$
DECLARE
  can_create boolean;
BEGIN
  SELECT r.rolsuper OR r.rolcreaterole INTO can_create FROM pg_catalog.pg_roles r WHERE r.rolname = current_user;
  IF coalesce(can_create, false) THEN
    IF NOT EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = 'forgebase_rls') THEN
      CREATE ROLE forgebase_rls NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS;
    END IF;
    IF NOT EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = 'forgebase_login') THEN
      CREATE ROLE forgebase_login NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS;
    END IF;
  END IF;
END
$fb$;

-- Direct logins see collections only through views here (§3.4).
CREATE SCHEMA IF NOT EXISTS api;
REVOKE ALL ON SCHEMA api FROM PUBLIC;

-- Which caller the policies see. Only sessions whose login role already
-- bypasses row-level security (the server's writer and read pools, admin
-- roles) may set the claims: a direct login's own `SET forgebase.claims`
-- is ignored. FB-153 adds the claims of named logins
-- (`_forgebase_db_logins`) to the ELSE branch.
CREATE OR REPLACE FUNCTION forgebase.claims() RETURNS jsonb
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = forgebase, pg_catalog AS $$
  SELECT CASE
    WHEN (SELECT r.rolsuper OR r.rolbypassrls FROM pg_catalog.pg_roles r WHERE r.rolname = session_user)
      THEN nullif(current_setting('forgebase.claims', true), '')::jsonb
    ELSE NULL::jsonb
  END
$$;

-- A rule's request value: 'auth.id', 'auth.roles', 'auth.<attribute path>',
-- 'tenant', 'realm', 'org'. SQL NULL when absent (also JSON null and
-- objects, as in Rust); roles, permissions and scopes are JSON arrays.
-- `scoped`: the collection is tenant-scoped (its roles come from the
-- tenant's organization).
CREATE OR REPLACE FUNCTION forgebase.req(path text, scoped boolean DEFAULT false) RETURNS jsonb
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = forgebase, pg_catalog AS $$
  SELECT CASE WHEN v IS NULL OR jsonb_typeof(v) IN ('null', 'object') THEN NULL ELSE v END
  FROM (
    SELECT CASE
      WHEN path IN ('tenant', 'realm', 'org') THEN c -> path
      WHEN path IN ('auth.id', 'auth.type', 'auth.collection', 'auth.authenticated') THEN c -> 'auth' -> substr(path, 6)
      WHEN path = 'auth.roles' THEN c -> 'eff' -> (CASE WHEN scoped THEN 'tenant' ELSE 'shared' END) -> 'roles'
      WHEN path = 'auth.permissions' THEN c -> 'eff' -> (CASE WHEN scoped THEN 'tenant' ELSE 'shared' END) -> 'allow'
      WHEN path = 'auth.scopes' THEN c -> 'scopes'
      WHEN path LIKE 'auth.%' THEN c -> 'attributes' #> string_to_array(substr(path, 6), '.')
    END AS v
    FROM (SELECT forgebase.claims() AS c) AS s
  ) AS x
$$;

CREATE OR REPLACE FUNCTION forgebase.auth_id() RETURNS text
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = forgebase, pg_catalog AS $$
  SELECT forgebase.claims() -> 'auth' ->> 'id'
$$;

CREATE OR REPLACE FUNCTION forgebase.tenant() RETURNS text
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = forgebase, pg_catalog AS $$
  SELECT forgebase.claims() ->> 'tenant'
$$;

-- The record action SELECT policies check: `list` or `view` for reads (a
-- direct login always lists), the write action inside a write.
CREATE OR REPLACE FUNCTION forgebase.read_action() RETURNS text
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = forgebase, pg_catalog AS $$
  SELECT coalesce(forgebase.claims() ->> 'action', 'list')
$$;

-- ForgeBase timestamps: `YYYY-MM-DDTHH:MM:SS.mmmZ`, UTC.
CREATE OR REPLACE FUNCTION forgebase.now_text() RETURNS text
LANGUAGE sql STABLE SET search_path = forgebase, pg_catalog AS $$
  SELECT to_char(now() AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"')
$$;

-- Permission pattern match for data keys `data:<collection>:<action>`
-- (forgebase-policy perm::matches): `*` one segment, a trailing `*` the
-- rest, case-insensitive, `read`/`write` aliases in the action segment.
CREATE OR REPLACE FUNCTION forgebase.perm_matches(pattern text, key text) RETURNS boolean
LANGUAGE plpgsql IMMUTABLE SET search_path = forgebase, pg_catalog AS $$
DECLARE
  p text[] := string_to_array(pattern, ':');
  k text[] := string_to_array(key, ':');
  np int := coalesce(array_length(p, 1), 0);
  nk int := coalesce(array_length(k, 1), 0);
  data boolean := k[1] = 'data' AND nk = 3;
BEGIN
  IF pattern = '' THEN
    RETURN false;
  END IF;
  FOR i IN 1..np LOOP
    IF i > nk THEN
      RETURN false;
    END IF;
    IF p[i] = '*' AND i = np THEN
      RETURN true;
    END IF;
    IF NOT (p[i] = '*'
            OR lower(p[i]) = lower(k[i])
            OR (data AND i = 3 AND ((p[i] = 'read' AND k[i] IN ('list', 'view'))
                                 OR (p[i] = 'write' AND k[i] IN ('create', 'update', 'delete'))))) THEN
      RETURN false;
    END IF;
  END LOOP;
  RETURN np = nk;
END
$$;

-- `granted`, `denied` or `none` for `data:<coll>:<action>`: deny patterns
-- win, then the collection's own grant in the claims (collection ACL and
-- API-key scopes, computed by ForgeBase), then allow patterns.
CREATE OR REPLACE FUNCTION forgebase.perm(coll text, action text, scoped boolean) RETURNS text
LANGUAGE plpgsql STABLE SECURITY DEFINER SET search_path = forgebase, pg_catalog AS $$
DECLARE
  c jsonb := forgebase.claims();
  e jsonb;
  k text := 'data:' || coll || ':' || action;
  own text;
BEGIN
  IF c IS NULL THEN
    RETURN 'none';
  END IF;
  e := c -> 'eff' -> (CASE WHEN scoped THEN 'tenant' ELSE 'shared' END);
  IF EXISTS (SELECT 1 FROM jsonb_array_elements_text(coalesce(e -> 'deny', '[]')) AS d(p) WHERE forgebase.perm_matches(d.p, k)) THEN
    RETURN 'denied';
  END IF;
  own := c -> 'colls' -> coll -> action ->> 'grant';
  IF own = 'denied' THEN
    RETURN 'denied';
  END IF;
  IF own = 'granted'
     OR EXISTS (SELECT 1 FROM jsonb_array_elements_text(coalesce(e -> 'allow', '[]')) AS a(p) WHERE forgebase.perm_matches(a.p, k)) THEN
    RETURN 'granted';
  END IF;
  RETURN 'none';
END
$$;

CREATE OR REPLACE FUNCTION forgebase.has_permission(coll text, action text, scoped boolean DEFAULT false) RETURNS boolean
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = forgebase, pg_catalog AS $$
  SELECT forgebase.perm(coll, action, scoped) = 'granted'
$$;

-- Steps ForgeBase decides in Rust before a query runs, as the claims say:
-- claims present, not suspended, realm valid, tenant membership (scoped).
CREATE OR REPLACE FUNCTION forgebase.gate(scoped boolean) RETURNS boolean
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = forgebase, pg_catalog AS $$
  SELECT c IS NOT NULL
     AND coalesce((c ->> 'suspended')::boolean, false) = false
     AND coalesce((c -> 'gate' ->> (CASE WHEN scoped THEN 'tenant' ELSE 'shared' END))::boolean, false)
  FROM (SELECT forgebase.claims() AS c) AS s
$$;

-- Record ACL ids of the caller for one collection and action.
DO $fb$
BEGIN
  IF NOT EXISTS (SELECT FROM pg_catalog.pg_type t JOIN pg_catalog.pg_namespace n ON n.oid = t.typnamespace
                 WHERE n.nspname = 'forgebase' AND t.typname = 'acl_ids') THEN
    CREATE TYPE forgebase.acl_ids AS (allow text[], deny text[]);
  END IF;
END
$fb$;

CREATE OR REPLACE FUNCTION forgebase.acl(coll text, action text) RETURNS forgebase.acl_ids
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = forgebase, pg_catalog AS $$
  SELECT ROW(
    ARRAY(SELECT jsonb_array_elements_text(coalesce(c -> 'colls' -> coll -> action -> 'allow', '[]'))),
    ARRAY(SELECT jsonb_array_elements_text(coalesce(c -> 'colls' -> coll -> action -> 'deny', '[]')))
  )::forgebase.acl_ids
  FROM (SELECT forgebase.claims() AS c) AS s
$$;

-- Value conversions for comparing request values with typed columns, the
-- way the server types the data core's bound parameter.
CREATE OR REPLACE FUNCTION forgebase.jtext(j jsonb) RETURNS text
LANGUAGE sql IMMUTABLE SET search_path = forgebase, pg_catalog AS $$
  SELECT CASE jsonb_typeof(j)
    WHEN 'string' THEN j #>> '{}'
    WHEN 'number' THEN j #>> '{}'
    WHEN 'boolean' THEN CASE WHEN j = 'true'::jsonb THEN '1' ELSE '0' END
  END
$$;

CREATE OR REPLACE FUNCTION forgebase.jnum(j jsonb) RETURNS numeric
LANGUAGE sql IMMUTABLE SET search_path = forgebase, pg_catalog AS $$
  SELECT CASE jsonb_typeof(j)
    WHEN 'number' THEN (j #>> '{}')::numeric
    WHEN 'boolean' THEN CASE WHEN j = 'true'::jsonb THEN 1 ELSE 0 END
    WHEN 'string' THEN CASE WHEN (j #>> '{}') ~ '^\s*[-+]?([0-9]+\.?[0-9]*|\.[0-9]+)([eE][-+]?[0-9]+)?\s*$'
                            THEN (j #>> '{}')::numeric END
  END
$$;

CREATE OR REPLACE FUNCTION forgebase.like_escape(s text) RETURNS text
LANGUAGE sql IMMUTABLE SET search_path = forgebase, pg_catalog AS $$
  SELECT replace(replace(replace(s, '\', '\\'), '%', '\%'), '_', '\_')
$$;

-- Rust's static comparison of two known values (forgebase-policy
-- rules::static_compare): booleans are 0/1, numbers sort before text, text
-- compares bytes, `~` is an ASCII case-insensitive substring test.
CREATE OR REPLACE FUNCTION forgebase.static_cmp(l jsonb, op text, r jsonb) RETURNS boolean
LANGUAGE plpgsql IMMUTABLE SET search_path = forgebase, pg_catalog AS $$
DECLARE
  lt text := jsonb_typeof(l);
  rt text := jsonb_typeof(r);
  ls text;
  rs text;
  o int;
BEGIN
  IF lt = 'boolean' THEN
    l := to_jsonb(CASE WHEN l = 'true'::jsonb THEN 1 ELSE 0 END);
    lt := 'number';
  END IF;
  IF rt = 'boolean' THEN
    r := to_jsonb(CASE WHEN r = 'true'::jsonb THEN 1 ELSE 0 END);
    rt := 'number';
  END IF;
  IF op IN ('~', '!~') THEN
    IF rt IS DISTINCT FROM 'string' THEN
      RETURN NULL;
    END IF;
    IF op = '~' AND lt = 'null' THEN
      RETURN NULL;
    END IF;
    rs := translate(r #>> '{}', 'ABCDEFGHIJKLMNOPQRSTUVWXYZ', 'abcdefghijklmnopqrstuvwxyz');
    ls := translate(CASE WHEN lt = 'null' THEN '' ELSE l #>> '{}' END,
                    'ABCDEFGHIJKLMNOPQRSTUVWXYZ', 'abcdefghijklmnopqrstuvwxyz');
    RETURN CASE WHEN op = '~' THEN strpos(ls, rs) > 0 ELSE strpos(ls, rs) = 0 END;
  END IF;
  IF rt = 'null' THEN
    RETURN CASE op WHEN '=' THEN lt = 'null' WHEN '!=' THEN lt <> 'null' END;
  END IF;
  IF lt = 'null' THEN
    RETURN CASE op WHEN '!=' THEN true END;
  END IF;
  IF lt = 'number' AND rt = 'number' THEN
    o := sign((l #>> '{}')::numeric - (r #>> '{}')::numeric)::int;
  ELSIF lt = 'string' AND rt = 'string' THEN
    o := CASE WHEN (l #>> '{}') COLLATE "C" < (r #>> '{}') COLLATE "C" THEN -1
              WHEN (l #>> '{}') COLLATE "C" > (r #>> '{}') COLLATE "C" THEN 1 ELSE 0 END;
  ELSIF lt = 'number' AND rt = 'string' THEN
    o := -1;
  ELSIF lt = 'string' AND rt = 'number' THEN
    o := 1;
  ELSE
    o := NULL;
  END IF;
  RETURN CASE op
    WHEN '=' THEN o = 0
    WHEN '!=' THEN o IS DISTINCT FROM 0
    WHEN '>' THEN o > 0
    WHEN '>=' THEN o >= 0
    WHEN '<' THEN o < 0
    WHEN '<=' THEN o <= 0
  END;
END
$$;

-- A comparison without record fields, decided at run time as Rust decides
-- it when the request is known (rules::Evaluator::compare): SQL NULL is an
-- absent value (unknown, except the presence tests `= null` / `= ""`), a
-- JSON array holds several values (only `?op` compares them: some item
-- matches), `any` is `?op`.
CREATE OR REPLACE FUNCTION forgebase.rule_cmp(l jsonb, op text, r jsonb, any_ boolean) RETURNS boolean
LANGUAGE plpgsql IMMUTABLE SET search_path = forgebase, pg_catalog AS $$
DECLARE
  present jsonb;
  item jsonb;
  unknown boolean := false;
  res boolean;
BEGIN
  IF l IS NULL OR r IS NULL THEN
    present := coalesce(l, r);
    IF present IS NOT NULL AND (jsonb_typeof(present) = 'null' OR present = '""'::jsonb) THEN
      IF op = '=' THEN
        RETURN true;
      ELSIF op = '!=' THEN
        RETURN false;
      END IF;
    END IF;
    RETURN NULL;
  END IF;
  IF jsonb_typeof(l) = 'array' THEN
    IF NOT any_ THEN
      RETURN NULL;
    END IF;
    FOR item IN SELECT e.v FROM jsonb_array_elements(l) AS e(v) LOOP
      IF jsonb_typeof(item) IN ('array', 'object') THEN
        item := 'null'::jsonb;
      END IF;
      res := forgebase.rule_cmp(item, op, r, false);
      IF res THEN
        RETURN true;
      ELSIF res IS NULL THEN
        unknown := true;
      END IF;
    END LOOP;
    RETURN CASE WHEN unknown THEN NULL ELSE false END;
  END IF;
  IF jsonb_typeof(r) = 'array' OR any_ THEN
    RETURN NULL;
  END IF;
  RETURN forgebase.static_cmp(l, op, r);
END
$$;

-- Only the roles that evaluate policies run these functions.
REVOKE ALL ON FUNCTION forgebase.claims(), forgebase.req(text, boolean), forgebase.auth_id(), forgebase.tenant(),
  forgebase.read_action(), forgebase.now_text(), forgebase.perm_matches(text, text), forgebase.perm(text, text, boolean),
  forgebase.has_permission(text, text, boolean), forgebase.gate(boolean), forgebase.acl(text, text),
  forgebase.jtext(jsonb), forgebase.jnum(jsonb), forgebase.like_escape(text), forgebase.static_cmp(jsonb, text, jsonb),
  forgebase.rule_cmp(jsonb, text, jsonb, boolean) FROM PUBLIC;

DO $fb$
DECLARE
  t record;
BEGIN
  IF EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = 'forgebase_rls') THEN
    GRANT USAGE ON SCHEMA forgebase, public TO forgebase_rls;
    GRANT USAGE, CREATE ON SCHEMA api TO forgebase_rls;
    GRANT EXECUTE ON FUNCTION forgebase.claims(), forgebase.req(text, boolean), forgebase.auth_id(), forgebase.tenant(),
      forgebase.read_action(), forgebase.now_text(), forgebase.perm_matches(text, text), forgebase.perm(text, text, boolean),
      forgebase.has_permission(text, text, boolean), forgebase.gate(boolean), forgebase.acl(text, text),
      forgebase.jtext(jsonb), forgebase.jnum(jsonb), forgebase.like_escape(text), forgebase.static_cmp(jsonb, text, jsonb),
      forgebase.rule_cmp(jsonb, text, jsonb, boolean) TO forgebase_rls;
  END IF;
  -- Direct logins read `api` views; PostgreSQL checks the functions those
  -- views and the table policies call against the caller, so the login
  -- group runs them too. They only read the caller's own claims (NULL for
  -- an untrusted session), so forged arguments reveal nothing.
  IF EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = 'forgebase_login') THEN
    GRANT USAGE ON SCHEMA api, forgebase TO forgebase_login;
    GRANT EXECUTE ON FUNCTION forgebase.claims(), forgebase.req(text, boolean), forgebase.auth_id(), forgebase.tenant(),
      forgebase.read_action(), forgebase.now_text(), forgebase.perm_matches(text, text), forgebase.perm(text, text, boolean),
      forgebase.has_permission(text, text, boolean), forgebase.gate(boolean), forgebase.acl(text, text),
      forgebase.jtext(jsonb), forgebase.jnum(jsonb), forgebase.like_escape(text), forgebase.static_cmp(jsonb, text, jsonb),
      forgebase.rule_cmp(jsonb, text, jsonb, boolean) TO forgebase_login;
  END IF;
  -- System tables: row-level security on, no policies, no grants to the
  -- RLS roles (§3.2). BYPASSRLS roles (server, reader, owner) are unaffected.
  FOR t IN
    SELECT c.oid::regclass AS rel
    FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
    WHERE n.nspname = 'forgebase' AND c.relkind IN ('r', 'p')
      AND pg_catalog.pg_has_role(current_user, c.relowner, 'USAGE')
  LOOP
    EXECUTE format('ALTER TABLE %s ENABLE ROW LEVEL SECURITY', t.rel);
  END LOOP;
END
$fb$;
