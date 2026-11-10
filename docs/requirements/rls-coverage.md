# Row-level security coverage (FB-95)

Human request (2026-11-11): "proper row-level security on every service,
Supabase-like, everything available".

This page says, for every ForgeBase service, which access rules apply today,
where they are enforced, what is missing, and how each gap gets fixed
(§1–2). §3 decides how ForgeBase rules become native PostgreSQL row-level
security (RLS) policies on the PostgreSQL backend of
[ADR-0028](../adr/0028-bundled-postgres.md), including direct `psql` and
driver logins. Work packages are in [`postgres-plan.md`](postgres-plan.md).

Sources: code read on 2026-11-11 at `ebb11ea` (file:line references below),
ADR-0003 §7, ADR-0008, ADR-0011, ADR-0018, ADR-0022, ADR-0023,
`edge-functions.md`.

## 0. Terms

ForgeBase checks access in layers. "Row-level security" in the human's sense
means all of them, on every service:

| Layer | Meaning | Where it lives |
|---|---|---|
| Isolation | realm → organization → project → environment (= **tenant**); data of one tenant is invisible to another | `AccessContext.{realm_id, org_id, tenant}`; policy step 2 (ADR-0008 §4, §8) |
| Permission | role-based keys such as `data:posts:list`, `engine:kv_read`; deny wins; API-key scopes | policy step 3 |
| Row rule | an expression per row: `owner = @request.auth.id`; compiled into the SQL `WHERE` (`Decision::Filter`) | collection rules, record ACL (policy step 5) |
| Field rule | which fields a caller may read or write | `FieldAccess`, `hidden`, password fields |
| Resource ACL | grants on one named thing (a bucket prefix, a record id, an environment) | `_forgebase_resource_permissions`, bucket ACLs |

"Privileged" below means `ctx.is_privileged()`: the system principal, an
administrator, or an admin service account
(`crates/forgebase-data/src/access.rs:225`). Privileged callers skip every
check, as Supabase's `service_role` does.

The Supabase reference point: PostgreSQL RLS on every table (including
`storage.objects` and `realtime.messages`), so the same policy language
covers rows, files and realtime channels; edge functions call the database
with the caller's token unless they use `service_role`.

## 1. Coverage today

✅ = enforced, ◐ = partial, ✗ = none.

| Service | Gate | Isolation | Row / item rules | Field rules | Gaps |
|---|---|---|---|---|---|
| Records and documents | ✅ `resolve()` per action (`forgebase-data/src/guard.rs:136-170`) | ✅ `_tenant = ?` on every read and write path (`guard.rs:175-195`); inserts stamped | ✅ rule predicate + record ACL allow/deny in the same SQL (`list.rs:161-173`, `records.rs:315-320`); written row re-checked (`records.rs:411-417`); expand filtered per target | ✅ `read_mask`, `check_writable`; masked fields refused in filter, sort, aggregate, search (`guard.rs:94-116, 255-299`) | G12, G13 |
| SQL editor | ✅ administrators only (`forgebase-policy/src/engine.rs:475-480`; `DefaultAuthorizer` too) | ✗ by design (admin tool) | ✗ by design | ✗ (password hashes, secrets readable) | G11, G12 |
| Schema, indexes | ✅ `ReadSchema` (`schema:read`, grantable), `ManageSchema`/`ManageIndexes` admin only | n/a | n/a | n/a | G12 |
| Key-value (HTTP) | ✅ `KvRead/Write/Admin` (`forgebase-kv/src/service.rs:198-229`) | ◐ scope `t:<tenant>` or shared `"default"` (`service.rs:181-196`) | ✗ all or nothing per tenant | n/a | G6, G7, G8 |
| Redis protocol (RESP) | ✅ API key or PAT, re-checked every 30 s (`forgebase-resp/src/auth.rs:130`) | ✗ sessions never get a tenant: `access_context(None)` (`auth.rs:149`) | ✗ (namespace narrowing by key scope only) | n/a | G6, G7 |
| Vector | ✅ `VectorRead/Write/Admin` (`forgebase-vector/src/service.rs:329`) | ◐ collections looked up by (tenant, name), `""` without a tenant; item SQL filters by `collection_id` only (`store.rs:232-546`) | ✗ | ✗ item `_data` returned in full | G9 |
| Graph | ✅ `GraphRead/Write` (`forgebase-graph/src/service.rs:50-57`) | ✅ `tenant = ?1` on every statement; ◐ shared `''` graph without a tenant | ✗ | ✗ | G10 |
| Object storage | ✅ owner, visibility, bucket ACL by key prefix, then `storage_*` permission (`forgebase-storage/src/service.rs:239-295`) | ✗ no tenant, realm or org; bucket names global (`service.rs:368-381`) | ◐ prefix ACL; `created_by` stored but never checked (`objects.rs:30`) | n/a | G1, G14, G15 |
| Queues and pub/sub | ✅ `Queue*` permissions (`forgebase-queue/src/service.rs:532-540`) | ✗ no tenant column | ✗ any `queue_consume` holder can pull or ack any subscription | ✗ record-source topics carry hidden fields | G2, G16 |
| Jobs and long tasks | ✅ `ReadJobs/EnqueueJob/ManageJobs/WorkJobs`, lease check (`forgebase-jobs/src/service.rs:151, 845`) | ✗ no tenant; `list` starts at `WHERE 1 = 1` (`service.rs:277`) | ✗ `created_by` never filtered | ✗ payload, result, error visible | G3, G4 |
| Realtime | ✅ scopes + List/View + filter per record subscription (`forgebase-realtime/src/conn.rs:324-350`); channel rules per message (`conn.rs:628`) | ✅ record events: cross-tenant guard (`conn.rs:561`); ✗ channel names have no tenant namespace | ✅ every delivery re-evaluated as the subscriber, against current rules (`conn.rs:524-611` → `forgebase-data/src/changes.rs:331-400`) | ✅ field mask per subscriber | G17 |
| Functions: host calls | ◐ HTTP invoke modes Public / Authenticated / `InvokeFunction` (`forgebase-functions/src/lib.rs:741-762`) | ✗ the function's identity has no tenant: tenant-scoped data refused | ✗ calls run as `service:function.<name>`, never as the caller (`lib.rs:169-172, 793-801`) | ✗ | G5 |
| Billing | ✅ own subject, or `billing:read/write` in the org or at realm level (`forgebase-billing/src/service.rs:402-500`); denied objects are 404 | ◐ realm fallback ignores the org's realm (`service.rs:441-449`); "all customers" lists not realm-filtered (`subscription.rs:385`, `billing.rs:1325`) | ✅ per subject | n/a | G18 |
| Credits | ✅ same pattern (`forgebase-credits/src/service.rs:911-1000`); minting realm-level only | ◐ same realm fallback (`service.rs:951-958`) | ✅ per subject | n/a | G18 |
| Entitlements | ✅ org goes through policy step 2 (membership, realm) | ✅ | ✅ per subject | n/a | — |
| Customize | ✅ realm must equal `ctx.realm_id` unless privileged (`forgebase-customize/src/service.rs:228-281`) | ✅ | n/a | n/a | — |
| Identity and auth | ✅ self-service ownership in SQL (sessions, keys); admin APIs `require_admin` (`forgebase-auth/src/service/mod.rs:1418`); impersonation with MFA, reason, TTL, admins not targetable | ◐ every administrator is global across realms; impersonation target's realm unchecked | ✅ users' records go through collection rules | ✅ `password`, `token_key` never returned | G19 |
| Admin and ops APIs | ✅ `ops_gate` for `/metrics`, `/api/system/info` (`forgebase-api/src/routes.rs:181-209`; loopback default) | n/a | n/a | n/a | — (opening `ops_access = public` is an operator choice) |
| Backups | ✅ privileged only (`forgebase-backup/src/http.rs:33-49`); no download route; restore CLI only | n/a (whole data directory, by design) | n/a | n/a | — |

## 2. Gaps and fixes

Severity: **High** = a non-privileged caller can read or change data of
another tenant, realm or user, or escalate; **Medium** = isolation depends on
configuration discipline or a missing rule type; **Low** = information leak
of metadata, or by-design behaviour that needs documenting.

| # | Severity | Gap (verified in code) | Fix | Package |
|---|---|---|---|---|
| G1 | High | Storage: bucket-level calls pass `key = None`, and `key.is_none_or(…)` (`storage/src/service.rs:264`) makes an ACL entry for **any** prefix match. An `admin` grant on `users/alice/` becomes admin of the whole bucket: update, `set_acl`, force delete. | Bucket-level operations match only entries with the empty prefix. Regression test per operation. | FB-106 |
| G2 | High | Queues: record-source topics build messages as `AccessContext::system()` (`queue/src/ops.rs:516-525`), so a message carries every non-password field (hidden and masked included) of every tenant, to anyone with `queue_consume`. | Evaluate at **delivery**, like realtime: a record-source subscription stores its owner's context (principal, realm, org, tenant); materialisation runs `view_snapshot` as that context and drops rows it cannot see, with the field mask applied. Creating one stays privileged. | FB-107 |
| G3 | High | Jobs: any `read_jobs` holder lists every job of every tenant with `payload`, `result`, `last_error` (`jobs/src/service.rs:277`; `model.rs:87-118`). The job channel authorizer needs `ReadJobs` plus the channel rule, not ownership (`jobs/src/hooks.rs:105-125`). | Jobs get `tenant` (stamped from the enqueuer) and are filtered by it; non-privileged callers see only jobs they created unless they hold `engine:read_jobs:all` (new key); channel subscription requires the same visibility. | FB-108 |
| G4 | High | Jobs: kinds are not reserved. With `enqueue_job` a caller can queue `function.invoke` (runs any function, skipping its invoke check: `functions/src/jobs.rs:18-37`), `storage.reconcile`, `billing.maintenance`, `customize.email.send`. | Handlers register as `reserved` (system and privileged only) or `public`. `function.invoke` from a caller checks `InvokeFunction` (or the function's access mode) at enqueue and runs with the enqueuer's recorded context. | FB-108 |
| G5 | High | Functions run as their own service identity `service:function.<name>`, never as the caller: a Public function gives any caller everything the function may do (confused deputy). Internal `invoke` (hooks, jobs) does no checks (`functions/src/lib.rs:725-738`). | Already specified for FB-93 (`edge-functions.md` §2): host calls run **as the caller** by default; `service_role` is an admin-granted capability. FB-108 adds: hooks and jobs invoke with an explicit, recorded context. | FB-93 (in flight), FB-108 |
| G6 | High | RESP sessions never carry a tenant (`resp/src/auth.rs:149`), so every RESP user shares the `"default"` scope, and so does every HTTP caller without a tenant. Tenant-scoped keys are unreachable over RESP. | API keys and PATs get an optional **tenant binding** (validated against policy step 2 when the key is created and when used); RESP sessions use it. A non-privileged caller without a tenant gets `tenant_required`, as for records; a realm-wide shared namespace becomes explicit (`shared.<ns>`, its own permission). | FB-104 |
| G7 | Medium | KV has no per-namespace rules; namespaces are narrowed only by API-key scopes (`resp/src/auth.rs:62-71`). `edge-functions.md` asks for namespace rules, deny by default. | Permission keys gain a resource segment: `engine:kv_read:<namespace>` (patterns with `*`); `engine:kv_read` keeps meaning "every namespace" for existing grants. Framework in FB-100, applied in FB-104. | FB-100, FB-104 |
| G8 | Low | KV `stats()` returns engine-wide totals to any `kv_admin` holder; LRU eviction is global, so one tenant can evict another's keys. | Stats per tenant for non-privileged callers; optional per-tenant memory budget `kv.tenant_max_bytes`. | FB-104 |
| G9 | Medium | Vector: no per-collection or per-item rules, item `_data` returned in full, only the collections table has a tenant column. Content copied from records into a vector collection is searchable without the record's rules. | Vector collection rules (list/view/create/update/delete over item metadata and `@request`, same grammar as collection rules) compiled into the search SQL as a pre-filter; optional `source_collection`: results are re-checked against that collection's View rule and field mask by key. Non-privileged callers without a tenant: `tenant_required`. | FB-100, FB-105 |
| G10 | Medium | Graph: no per-label or per-edge-type rules; properties returned in full; callers without a tenant share the `''` graph. | Label rules (read/write over node and edge properties, `@request`), compiled into the traversal SQL; `tenant_required` for non-privileged callers. | FB-100, FB-105 |
| G11 | Medium | SQL editor (admins only): admins read every tenant, password hashes and secrets kept in plain tables, e.g. the storage URL signing key (`storage/src/service.rs:707-721`), which lets anyone with SQL access forge signed URLs. | Seal the storage signing key with the auth keyring (as billing provider secrets are). Result masking of `password`/`token_key` columns in the SQL editor unless the admin ticks "show secrets" (audited). On PostgreSQL: the editor runs as a role without `SELECT` on secret columns, and a **"run as"** selector (role + claims of a chosen user) for testing rules (§3.5). | FB-101, FB-106, FB-119 |
| G12 | Low | `describe_table` needs only `schema:read` (grantable to non-admins) and does not hide `_forgebase_*` tables, unlike `list_tables` (`data/src/sql.rs:273-312`). | Same system-table filter for non-privileged callers. | FB-101 |
| G13 | Low | Record versions are read by id; the visibility pre-check runs only when there is a predicate or a tenant-bound caller (`data/src/records.rs:823-847`). History of a deleted record is then readable by any caller with `View` on an unrestricted collection. Cascades on referencing collections skip their Delete/Update rules (`records.rs:712-752`). | Always check visibility (live row, or the tombstone's tenant and the predicate over the last version). Cascades: document as the same semantics as PostgreSQL foreign-key actions, which also bypass RLS. | FB-101 |
| G14 | Medium | Storage has no tenant, realm or org scope; bucket names are global; object ownership (`created_by`) is never used. | Tenant-scoped buckets (names unique per tenant, `_tenant` column), and **object rules** per bucket in the rule grammar over object fields (`key`, `created_by`, `size`, `content_type`, path segments like Supabase's `storage.foldername`) with `@request`, compiled into the object SQL. | FB-100, FB-106 |
| G15 | Low | A signed URL is checked when issued, never when used, until it expires (`storage/src/signed.rs:178-184`). | By design (same as S3 and Supabase). Document; keep the short default lifetime; rotating the signing key revokes all. | FB-106 (docs) |
| G16 | Medium | Queues: no tenant scope, no per-topic permissions, no subscription ownership. | Topics optionally tenant-scoped; permission keys `engine:queue_publish:<topic>`, `engine:queue_consume:<subscription>`; a subscription's creator owns it. | FB-100, FB-107 |
| G17 | Medium | Realtime channel names have no tenant or realm namespace; isolation depends entirely on channel rules. | For tenant-bound callers channel names are implicitly prefixed `t:<tenant>:`; channel rules can use `@request.tenant`; an explicit `global:` prefix for cross-tenant channels, privileged to publish. | FB-102 |
| G18 | High | Billing and credits: when the org check fails, `check_for` falls back to the realm-level permission without checking that the org belongs to the caller's realm (`billing/src/service.rs:441-449`, `credits/src/service.rs:951-958`); "all customers" lists are not realm-filtered. A realm-level billing manager can act on another realm's orgs. | Fall back to realm level only when the subject (org or principal) is in `ctx.realm_id`; filter lists by realm. | FB-102 |
| G19 | Medium | Every administrator is a cross-realm superuser; impersonation does not check the target's realm. | Realm-scoped administrators (realm bindings with an `identity:admin` key) for the identity admin APIs; impersonation limited to the admin's realms. Platform admins stay global. | FB-103 |

Already right, keep as they are: record and document rules, field masks and
the write re-check; realtime's per-subscriber re-evaluation; entitlements and
customize scoping; self-service ownership checks in auth; the SQL editor and
backups being administrator-only.

Every fix lands with a test in `crates/forgebase-server/tests/security_bypass.rs`
(the FB-80 suite) that fails before the fix.

## 3. Native PostgreSQL RLS

### 3.1 Decision

On the PostgreSQL backend, ForgeBase **compiles its rules to native
PostgreSQL policies**. ForgeBase rules (`_forgebase_collection_policies`,
field rules, record ACL) stay the single source of truth; the policies are
generated from them, never edited by hand.

Why:

- **Direct access**: once PostgreSQL is there, people will connect `psql`,
  BI tools and drivers. Without native policies those logins would either see
  everything or nothing.
- **Defence in depth**: in `enforce` mode a bug in how the data core builds
  its `WHERE` clause can no longer leak rows; PostgreSQL filters them again.
- It is what "Supabase-like" means to users: RLS that holds whatever path
  the query takes.

Why not *only* native policies: SQLite has none, so the data core must keep
enforcing rules itself; policy steps 1–4 and 6 (suspension, memberships,
deny-wins permissions, entitlement and quota hooks) are computed in Rust
from the policy snapshot and are not re-implemented in SQL.

Modes, `[database.postgres] native_rls`:

| Mode | Policies installed | API path | Direct logins |
|---|---|---|---|
| `off` | none (RLS enabled, deny all, on every table) | data core only, as on SQLite | administrator logins only |
| `logins` (default) | yes | data core only (server role bypasses RLS) | row and field rules apply |
| `enforce` | yes | data core **and** PostgreSQL (non-privileged requests run under the RLS role) | row and field rules apply |

`enforce` becomes the default after FB-118's differential tests and an
overhead measurement (expected: one `SET LOCAL` round trip per transaction
plus policy evaluation).

### 3.2 Roles and schemas

System tables live in schema `forgebase`, collection tables in `public`;
the server's `search_path` is `forgebase, public` (ForgeBase SQL uses
unqualified `_forgebase_*` names, which keep working).

| Role | Login | Attributes | Used for |
|---|---|---|---|
| `forgebase_super` | socket only | superuser | `initdb` bootstrap, backups, upgrades, extensions |
| `forgebase_owner` | no | `BYPASSRLS` | owns every schema, table, view, function |
| `forgebase_server` | yes (socket / loopback) | `BYPASSRLS`, member of `forgebase_owner` and `forgebase_rls` | the server's writer pool |
| `forgebase_reader` | yes | `BYPASSRLS`, `SELECT` only, read-only transactions | the read pool |
| `forgebase_rls` | no | **no** `BYPASSRLS`; DML on collection tables; owns the `api` views | requests in `enforce` mode, direct logins |
| `forgebase_sql_admin` | no | `BYPASSRLS`, DDL/DML on `public`, `SELECT` on `forgebase` except secret columns; **not** superuser, no `pg_read_server_files` / `pg_execute_server_program` | the SQL editor; admin logins |
| `fbu_<name>` | yes | `CONNECTION LIMIT`, `statement_timeout`, `VALID UNTIL` | named direct logins (§3.5) |

- Every collection table: `ENABLE` and `FORCE ROW LEVEL SECURITY`.
  `BYPASSRLS` roles are the only ones that skip it (PostgreSQL applies that
  attribute even to forced tables).
- System tables: RLS enabled, no policies, no grants to `forgebase_rls` or
  `fbu_*`; reachable only through ForgeBase APIs (and admin roles).

### 3.3 Who is the caller inside PostgreSQL

All policies read the caller from one function:

```sql
CREATE FUNCTION forgebase.claims() RETURNS jsonb
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = forgebase, pg_catalog AS $$
  SELECT CASE
    WHEN session_user = 'forgebase_server'
      THEN nullif(current_setting('forgebase.claims', true), '')::jsonb
    ELSE (SELECT claims FROM forgebase._forgebase_db_logins WHERE role_name = session_user)
  END
$$;
```

- **API path** (`enforce`): for a non-privileged request the data core runs
  its transaction as `SET LOCAL ROLE forgebase_rls` plus
  `set_config('forgebase.claims', $1, true)`. Claims are computed in Rust:
  principal, realm, org, tenant, roles, auth-record attributes, the
  **effective permission patterns** after deny-wins and API-key scopes, the
  action (`list` or `view`), and suspension. Only the server's own session
  is trusted to set them; ForgeBase SQL never lets user input reach
  `set_config`.
- **Direct logins**: a login can run `SET forgebase.claims = …` itself, so
  for any session other than `forgebase_server` the setting is ignored and
  the claims come from `_forgebase_db_logins`, which ForgeBase rewrites after
  every policy snapshot reload (the same moment the in-process cache is
  invalidated, ADR-0008 §7). An unknown login gets `NULL` claims: every
  policy fails, it sees nothing.
- Helper functions on top: `forgebase.auth_id()`, `forgebase.tenant()`,
  `forgebase.has_permission(key)` (pattern match against the claims list),
  `forgebase.acl(collection, action) → text[]` (record ACL allow ids, deny
  ids), all `STABLE`, `SECURITY DEFINER`, fixed `search_path`.

### 3.4 How a rule becomes policies

For collection `C`, regenerated in the same transaction as any change to
`C`'s schema, rules, field rules or tenancy (a hook the data core calls
inside its `SchemaChange` transaction, implemented by `forgebase-policy`).
Names start with `fb_`; user DDL on `fb_*` policies is refused.

| Policy | Kind | Command | Expression |
|---|---|---|---|
| `fb_tenant` | restrictive | all | `_tenant = forgebase.tenant()` (tenant-scoped collections) |
| `fb_gate` | restrictive | all | caller authenticated where the rule needs it, not suspended, realm and org valid (claims present) |
| `fb_acl_deny` | restrictive | all | `NOT (id = ANY ((forgebase.acl('C', <action>)).deny))` |
| `fb_perm_<cmd>` | permissive | per command | `forgebase.has_permission('data:C:<action>')` |
| `fb_rule_<cmd>` | permissive | per command | the compiled rule (below) |
| `fb_acl_<cmd>` | permissive | per command | `id = ANY ((forgebase.acl('C', <action>)).allow)` |

This mirrors ADR-0008 §8: a permission grants, else a rule or an ACL entry
grants; a deny always wins; tenancy always applies.

- `SELECT` uses the **view** rule when the claims say `action = view` and
  the **list** rule otherwise (direct logins always "list", the conservative
  choice). `INSERT` gets `WITH CHECK (create rule)`; `UPDATE` gets `USING`
  and `WITH CHECK` (update rule), which is the data core's write re-check;
  `DELETE` gets `USING (delete rule)`. A `null` rule (locked) produces no
  permissive policy for that command; `""` produces `USING (true)`.
- **Compiling the expression**: the same `forgebase-query` compiler, in a new
  *native* mode. Local field paths render exactly as today (so indexes are
  used: `owner = forgebase.auth_id()` hits the `owner` index).
  `@request.auth.id` → `forgebase.auth_id()`; `@request.auth.<field>` →
  `(forgebase.claims()->'attributes'->>'<field>')` cast to the field type;
  `@request.auth.roles ?= 'x'` → an `EXISTS` over the claims array;
  `@request.tenant|realm|org` → claims; `@now` → the current UTC time in
  ForgeBase's text format. Absent request values are SQL `NULL`, which gives
  the same three-valued result as the Rust partial evaluation (ADR-0008 §6)
  because that evaluation already uses the compiler's SQL semantics.
- **Relation paths** in rules (`author.team = …`) would run as the caller
  inside PostgreSQL and hit the target table's RLS (and can recurse). The
  data core evaluates them with full visibility, so native mode wraps each
  cross-table lookup in a generated `SECURITY DEFINER` function
  (`forgebase.rel_<hash>(id)`); local predicates stay inline.
- **Field rules**: columns nobody may read through a rule (`password`,
  `token_key`, `hidden`, read rule `null`) are not granted to `forgebase_rls`.
  For direct logins each collection also gets a view `api.<collection>`
  (`security_barrier`, owner `forgebase_rls`, so the forced table policies
  apply to it): request-dependent field read rules become
  `CASE WHEN <rule> THEN col END`; writes go through generated `INSTEAD OF`
  triggers that refuse fields the caller may not write. Direct logins get
  `USAGE` on schema `api` only.
- **Not covered natively**, stated plainly: entitlement and quota hooks
  (steps 4 and 6) do not run for direct SQL, so direct logins are not metered
  (creating one needs the `db.direct_access` entitlement when entitlements
  are on); views of tenant data stay closed to non-privileged callers as
  today; KV, vector, graph, storage, queues and jobs tables are API-only
  (not exposed through `api`). Exposing `storage.objects`-style views with
  object rules is a later step once G14's object rules exist.
- **Policies users write themselves** (SQL editor) are allowed. Restrictive
  ones narrow both the API (in `enforce`) and direct logins. Permissive ones
  widen **direct logins only**; on the API path the data core's predicate
  still applies. The Studio lists them separately with that warning.

### 3.5 Direct logins

- `forgebase db login create <name> --as <principal> [--tenant <id>]
  [--readonly] [--admin]` and `POST /api/v1/system/db-logins` (administrators
  only; MFA when enabled; audit-logged). The login is bound to one ForgeBase
  principal (a user or service account) and at most one tenant: a raw
  PostgreSQL connection cannot send `X-ForgeBase-Tenant`.
- `--readonly` makes it a member of nothing writable; `--admin` makes it a
  member of `forgebase_sql_admin` (the SQL editor's rights) and requires a
  platform administrator.
- Passwords: SCRAM, generated, shown once; rotation and expiry (`VALID
  UNTIL`); `CONNECTION LIMIT` (default 10), `statement_timeout` (default
  30 s) and `idle_in_transaction_session_timeout` per role.
- Network: Unix socket by default. `[database.postgres] listen` opens TCP;
  ForgeBase generates `pg_hba.conf` (`local` for its own roles; `hostssl …
  scram-sha-256` for `fbu_*` when off loopback, with a ForgeBase-generated
  or configured certificate). Superuser and server roles are never allowed
  over TCP from other hosts.
- `forgebase db psql [--as <login>]` starts the bundled `psql` over the socket
  (default: an admin session as `forgebase_sql_admin`).
- **SQL editor "run as"**: an administrator picks a user (and tenant); the
  statement runs as `forgebase_rls` with that user's claims, so rules can be
  tested in the editor, as in Supabase's role selector.

### 3.6 Extensions

`CREATE EXTENSION` runs only through `POST /api/v1/system/extensions`
(administrators, as `forgebase_super`) from the allow list of ADR-0028 §1.
The SQL editor refuses `CREATE/ALTER/DROP EXTENSION`.

### 3.7 Tests

- **Differential**: for generated collections, rules, ACL entries and
  principals, the rows a caller gets through the API (data core only) equal
  the rows `forgebase_rls` gets with that caller's claims (native only), for
  list, view, create, update and delete. This is the test that keeps the
  rule compiler and the native compiler equivalent.
- **Spoofing**: a direct login running `SET forgebase.claims`, `SET ROLE`,
  `set_config`, creating `fb_*` policies, calling the `SECURITY DEFINER`
  helpers with forged arguments, or reading `forgebase.*` tables gets
  nothing.
- Every gap in §2 has a bypass test on both backends.
