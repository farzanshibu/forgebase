# ADR-0008: Authorization, tenancy and the evaluation pipeline

- Status: accepted
- Date: 2026-11-09
- Work package: FB-21 (MDF-21), with the addenda of
  `docs/requirements/authz-entitlements-billing.md` §B, §C and §F.
- Builds on: ADR-0003 §7 (`AccessContext`, `Authorizer`), ADR-0004 (identities,
  scopes, mailer).
- Leaves to later cards: entitlements and quotas behind the hooks of §8
  (FB-24), realm-scoped identities (FB-25), the policy editor and Auth Studio
  (FB-23).

## 1. Crates and what changed for other packages

New crate `forgebase-policy` (depends on `forgebase-data`, `forgebase-auth`
for the mailer and random tokens, `forgebase-query`, `forgebase-db`). It is a
separate crate, not part of `forgebase-data`, because it owns its own tables,
HTTP routes and the tenancy layer; the data core only knows the `Authorizer`
trait and enforces tenancy and field masks itself (§4, §6), so it stays safe
with any authorizer.

```text
forgebase-server ─► forgebase-api ─► forgebase-policy ─► forgebase-auth ─► forgebase-data
```

Source-compatible changes in `forgebase-data::access` (what KV, graph and jobs
build on):

| Change | Compatibility |
|---|---|
| `Authorizer::field_access(&self, ctx, &Collection) -> FieldAccess` | new method **with a default** (no restriction); `authorize` unchanged |
| `Principal::Service { id, admin }` | `Principal` was already `#[non_exhaustive]`; admin service accounts are privileged (`is_privileged`) |
| `AccessContext` gains `realm_id`, `org_id`, `scopes`, `roles`, `attributes` | constructors set defaults; use `with_realm`, `with_org`, `with_tenant`, `with_scopes`, `with_roles`, `with_attributes` (nothing in the workspace built it with a struct literal) |
| `Action` | **unchanged** (engine workers append their own variants; the policy engine names any variant generically, §5) |
| `forgebase-query::CompileOptions::field_guard` | new field, `None` in `Default` |
| `forgebase-auth`: service accounts map to `Principal::Service`; `Authenticated::access_context` copies scopes; `MailKind::Invitation`; `AuthService::mailer()` public | additive |

`forgebase-api` dropped FB-11's `hide_collection_existence` response rewrite:
the data core now resolves a collection and authorises in one step
(`DataService::resolve`) and answers callers who are not privileged the same
denial for a missing collection as for a forbidden one, with the name as the
caller spelled it.

## 2. Subjects: B2C and B2B

A user has a personal scope and can belong to any number of organizations.
Every check names its subject: the organization it acts in (an org route,
the organization owning the selected tenant, `X-ForgeBase-Org`), else the
caller's personal scope (`SubjectRef::Principal`); `AccessContext::subject()`
gives the same for the request. Hooks (§8) bill and meter against it.
Projects are owned by an organization or by one user (personal project: the
owner holds every permission on it).

## 3. Realms and the hierarchy

```text
realm ─► organization ─► project ─► environment (= tenant)
                     └─► team (tree)
```

- A **realm** is the top isolation level (§F of the requirements): its own
  organizations, realm-level roles and role bindings. Migration 0004 seeds
  realm `default`; everything that exists belongs to it.
- `AccessContext::realm_id` defaults to `default`; `X-ForgeBase-Realm` selects
  another. **Cross-realm access is denied to everyone but platform
  super-admins**: until FB-25 makes identities realm-scoped, every user and
  service account belongs to `default` (`engine::home_realm`), so a request
  for any other realm by them is refused at step 2. FB-25 replaces
  `home_realm` with the identity's realm. **Done (FB-25, ADR-0015 §3):**
  `home_realm(snap, ctx)` reads the auth service's `RealmDirectory`
  (attached with `PolicyService::attach_realms`); anonymous callers act in
  the realm they select.
- Roles exist at **realm scope** (`org_id` NULL: usable in every organization
  of the realm and in realm bindings) and at **org scope** (an org's own
  roles, which shadow realm roles of the same key inside that org).
  Organizations keep their own memberships.
- The FB-20 auth tables are not realm-scoped (FB-25 does that).

## 4. Tenancy in the data core

A collection created with `options.tenant_scoped = true` gets a `_tenant
TEXT NOT NULL` column (immutable flag: `tenant_scope_immutable`). The tenant is
a project environment id. The data core, independent of the authorizer:

- adds `_tenant = <ctx.tenant>` to every statement on such a collection (list,
  count, aggregate, explain, get, update, delete, versions, expand, the
  write re-check) — a known id of another tenant is `record_not_found`;
- stamps `_tenant` on insert (no tenant → `tenant_required`; clients cannot
  set it, it is a read-only key);
- refuses non-privileged callers without a tenant (`tenant_required`);
- makes unique indexes per tenant (`UNIQUE (_tenant, field)`), so other
  tenants' values neither collide nor leak through "already in use";
- checks relation targets inside the record's tenant, and refuses relations
  from a shared collection into a tenant-scoped one (schema error);
- closes views that read tenant-scoped tables (`reads_tenant_data`, computed
  from the view query's identifiers) to non-privileged callers;
- never returns `_tenant` to non-privileged callers; privileged callers
  without a tenant see all tenants (Database Studio).

Accepted limitation: record ids are global primary keys, so creating a record
with a client-chosen id that another tenant uses fails as a duplicate id (the
caller already knows the id; nothing else is revealed).

## 5. Permissions, roles, deny

Keys are `resource:action[:…]` — `users:invite`, `teams:members:manage`,
`projects:share`, `billing:manage`, `analytics:export`, `api_keys:revoke`,
`ai:generate`, `org:<feature>:<permission>` — plus ForgeBase's own:
`org:read|update|delete`, `roles:read|manage`, `acl:manage`,
`data:<collection>:<list|view|create|update|delete>` (aliases `read`, `write`),
`engine:<action>` (any data-core `Action` variant in snake case, e.g.
`engine:kv_read`, so KV/graph/jobs actions are grantable without code here),
`schema:read`. Patterns use `*` for one segment and a trailing `*` for the
rest. **Checks are always by permission key, never by role name.**

Seeded roles (realm `default`, editable by super-admins): **Super Admin**
(documentation row, `assignable = 0`: it can never be granted through a
membership — the capability is being a FB-20 administrator or an admin
service account), **Owner** (`*`), **Admin** (all but `org:delete`),
**Manager**, **Developer**, **Billing Manager**, **Analyst**, **Member**,
**Viewer**, **Guest** (`org:read` only; shared resources come from the ACL).
Engine permissions (`engine:kv_read|kv_write|kv_admin`, `engine:graph_read|graph_write`,
`engine:read_jobs|enqueue_job|manage_jobs|work_jobs`): of the seeded roles only
Owner (`*`) and Developer (`engine:*`) hold them; other roles get them through
custom roles or a super-admin edit of the seeded role (extending the seed is a
follow-up migration, since 0004 has shipped). The exact patterns are in
`migrations/system/0004_policy.sql`.

Engine actions reach the authorizer with no collection; because KV and graph
scope their data by `ctx.tenant`, the tenant is validated as strictly as for
records: it must exist in the caller's realm, belong to the active
organization when one is set, and be the caller's (active membership, personal
ownership, or an ACL grant on the environment or its project). Engines treat
`Decision::Filter` as a denial; the policy engine only ever answers
`Allow`/`Deny` for them.

Sources of a caller's effective permissions in an org: realm bindings,
the active membership's roles and role sets, every team they are in **and
every ancestor team** (team roles and team denies are inherited down the
tree), and explicit overrides (per principal, org-wide or realm-wide).
Every source has allow and deny lists; **a deny from any source wins over
every grant**. Org roles reach only tenant-scoped data of that org's tenants;
shared collections see realm-level grants only.

Escalation guard: nobody but a super-admin can grant — via membership, team,
role definition, role set, override, invitation or ACL entry — a pattern they
do not hold themselves; seeded roles change only by super-admins.

Account suspension (`_forgebase_suspensions`, optional end time) denies the
principal everywhere at step 1; a membership can also be suspended in one org
(step 2).

## 6. Rules

Collection rules (PocketBase-style, `_forgebase_collection_policies`) for
list, view, create, update and delete: `null` = locked (privileged only —
`DefaultAuthorizer` semantics), `""` = everyone, otherwise an expression in the
data core's filter grammar plus `@request.auth.{id,type,collection,
authenticated,roles,permissions,scopes,<auth record field>}`,
`@request.tenant|realm|org`, `@now`. Caller parameters (`{:x}`) are refused in
rules. A rule is parsed once and, per request, partially evaluated in
three-valued logic: request-only parts are decided in Rust with the
compiler's SQL semantics; the rest becomes a `Decision::Filter` predicate with
request values as bound parameters, compiled into the same SQL statement as
the caller's filter. An absent or null request value makes its comparisons
*unknown* (rendered as SQL `NULL`), except explicit `= null` / `= ""`
presence tests, so `owner = @request.auth.id` never matches rows with an empty
owner for anonymous callers and negation cannot turn unknown into a grant.
Unknown at the top denies. A stored rule that no longer compiles (e.g. it
names a dropped field) denies (`the access rule cannot be evaluated`), never
a schema-leaking 400. Rules are validated against the collection schema when
saved.

Field rules (`fields: {name: {read, write}}`) may use request values only.
For a caller they yield `FieldAccess`: unreadable fields are treated like
`hidden` — left out of records, versions and expansions, and refused (as an
unknown field) in filter, sort, group-by, aggregates, relation paths and
full-text search over them; read-only fields cannot be set on create or
changed on update. Relation paths in a caller's filter may enter another
collection only if the caller can list it without a row restriction.

Rules are trusted admin input: ABAC on fields of the caller's own auth record
(`@request.auth.department`) is only as safe as the write rules that protect
those fields.

## 7. The model in memory and cache invalidation

All tables of migration 0004 load into one immutable `Snapshot`; the
synchronous `Authorizer` reads only the snapshot (no I/O per check).
Effective permissions are cached per (principal, realm, org) inside the
snapshot. Every write through `PolicyService` commits and then swaps in a
freshly loaded snapshot: that swap is the invalidation, so a change to any
role, role set, membership, team, override, ACL entry, suspension or rule is
never served stale by this process. Limitation: another process writing the
same tables (none today) is not observed until its own reload; multi-node
deployments need a change feed (FB-40).

## 8. The evaluation pipeline and the FB-24 hooks

Every protected operation runs the same steps in this order; the first
failure denies and is written to `_forgebase_authz_audit` with its step
(through a bounded channel and a background writer, so the synchronous path
never blocks; a full queue drops with a warning):

| # | Step | What decides |
|---|---|---|
| 1 | authentication | anonymous where a caller is required; suspended account |
| 2 | tenant | realm (cross-realm), organization, tenant ownership, active membership |
| 3 | permission | effective permissions; deny wins; API-key scopes |
| 4 | entitlement | `EntitlementHook::check` (FB-24) |
| 5 | resource | resource ACL (`_forgebase_resource_permissions`; deny wins; an environment inherits its project's entries), then collection rules and record-level ACL entries |
| 6 | quota | `QuotaHook::check` (FB-24) |

Platform super-admins skip the pipeline. For record calls a missing
permission is not yet a denial: step 5 can still grant through a rule or an
ACL entry; for other operations a permission or an ACL grant is required.

```rust
pub struct HookRequest<'a> {
    pub ctx: &'a AccessContext,          // caller, realm, org, tenant, roles
    pub subject: SubjectRef,             // the org the check acts in, else the caller (billing subject)
    pub permission: &'a str,             // e.g. "data:posts:create", "projects:create", "engine:kv_write"
    pub resource: Option<&'a ResourceRef>,
}
pub enum HookOutcome { Allow, Deny(String) }
pub trait EntitlementHook: Send + Sync + Debug { fn check(&self, req: &HookRequest<'_>) -> HookOutcome { Allow } }
pub trait QuotaHook: Send + Sync + Debug { fn check(&self, req: &HookRequest<'_>) -> HookOutcome { Allow } }
```

Both default to allow (`AllowAll`); `PolicyService::set_hooks` installs FB-24's.
Contract: hooks run on the request path inside the synchronous authorizer, so
they answer from memory (FB-24 keeps its own cache); step 6's `check` is the
admission decision, while atomic reservation, commit and release of usage are
FB-24's API inside the operation's transaction (FB-24 may add them to
`QuotaHook` as default methods). New hook methods always get defaults.

## 9. Storage (migration 0004)

`_forgebase_realms` (seeded `default`), `_forgebase_orgs`,
`_forgebase_projects` (org- or user-owned), `_forgebase_environments`,
`_forgebase_roles` (seeded), `_forgebase_role_sets`,
`_forgebase_realm_bindings`, `_forgebase_memberships`, `_forgebase_teams`
(parent links), `_forgebase_team_members`, `_forgebase_permission_overrides`,
`_forgebase_resource_permissions`, `_forgebase_suspensions`,
`_forgebase_collection_policies`, `_forgebase_invitations` (SHA-256 of a
32-byte random token, 7 days, single use, accepted only by the user whose
auth record has the invited email), `_forgebase_authz_audit`. Principals are
`user:<collection>/<id>` and `service:<id>`. Forward-only, pre-migration
snapshot (ADR-0001 §4).

## 10. HTTP

The tenancy layer runs after authentication and reads `X-ForgeBase-Realm`,
`X-ForgeBase-Org`, `X-ForgeBase-Tenant`; it applies step 2 (403 with
`details.step`), then fills `realm_id`, `org_id`, `tenant`, `roles` and, for
users, `attributes` (their auth record without `token_key`). Anonymous callers
may name a tenant (public rules); an unknown tenant just matches no rows.

Routes under `/api/v1` (all in the OpenAPI document, tags `tenancy` and
`authorization`): `realms`, `orgs[/{org}]`, `orgs/{org}/projects`,
`projects[/{project}[/environments]]`, `orgs/{org}/members[/remove|/status]`,
`orgs/{org}/teams`, `teams/{team}[/members[/remove]]`,
`orgs/{org}/roles[/{key}]`, `orgs/{org}/role-sets`, `roles[/{key}]`,
`role-sets`, `realm-bindings`, `overrides[/{id}]`, `acl[/{id}]`,
`suspensions[/lift]`, `policies/{collection}`, `orgs/{org}/invitations`,
`invitations/accept`, `authz/me`, `authz/check` (dry run), `authz/audit`.

## Consequences

- Checks cost no I/O; a write reloads the whole model (fine for thousands of
  rows; FB-81 measures; an incremental reload is a follow-up).
- Record-level ACL entries become `id IN (...)` terms of the predicate; many
  thousands of entries per caller and collection would need a join instead.
- Follow-ups: policy editor (FB-23); realm-scoped identities and
  `home_realm` (FB-25); entitlements and quotas (FB-24); rewriting rule paths
  on field renames (today a rule naming a renamed field denies until it is
  updated); a change feed for multi-process cache invalidation.
