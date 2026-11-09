-- 0004_policy: authorization and tenancy (FB-21, ADR-0008).
--
-- Hierarchy: realm -> organization -> project -> environment. An environment
-- id is the tenant id of the data core (`AccessContext::tenant`, the `_tenant`
-- column of tenant-scoped collections). A project is owned by an organization
-- (B2B) or by one user (B2C personal scope).
--
-- Principals are strings: `user:<auth collection>/<record id>` or
-- `service:<service account id>`. Platform super-admins are FB-20
-- administrators (and admin service accounts); they are never an org role.
--
-- Permissions are keys `resource:action[:...]` (`projects:create`,
-- `teams:members:manage`, `org:<feature>:<permission>`, `data:<collection>:<list|view|create|update|delete>`,
-- `engine:<action>`), with `*` wildcards. Allow and deny lists everywhere;
-- deny always wins. Only the policy service writes these tables; every write
-- invalidates its permission cache.
--
-- Times are INTEGER Unix seconds. Lists are JSON arrays of strings.

CREATE TABLE _forgebase_realms (
    id          TEXT    NOT NULL PRIMARY KEY CHECK (length(id) BETWEEN 1 AND 64),
    name        TEXT    NOT NULL CHECK (length(name) BETWEEN 1 AND 100),
    settings    TEXT    NOT NULL DEFAULT '{}' CHECK (json_valid(settings)),
    created_at  INTEGER NOT NULL
) STRICT;

INSERT INTO _forgebase_realms (id, name, settings, created_at) VALUES ('default', 'Default', '{}', unixepoch());

CREATE TABLE _forgebase_orgs (
    id          TEXT    NOT NULL PRIMARY KEY CHECK (length(id) BETWEEN 1 AND 64),
    realm_id    TEXT    NOT NULL REFERENCES _forgebase_realms(id) ON DELETE RESTRICT,
    slug        TEXT    NOT NULL CHECK (length(slug) BETWEEN 1 AND 64),
    name        TEXT    NOT NULL CHECK (length(name) BETWEEN 1 AND 100),
    created_by  TEXT    NOT NULL,
    created_at  INTEGER NOT NULL,
    UNIQUE (realm_id, slug)
) STRICT;

CREATE TABLE _forgebase_projects (
    id          TEXT    NOT NULL PRIMARY KEY CHECK (length(id) BETWEEN 1 AND 64),
    realm_id    TEXT    NOT NULL REFERENCES _forgebase_realms(id) ON DELETE RESTRICT,
    org_id      TEXT    REFERENCES _forgebase_orgs(id) ON DELETE CASCADE,
    owner_user  TEXT,
    slug        TEXT    NOT NULL CHECK (length(slug) BETWEEN 1 AND 64),
    name        TEXT    NOT NULL CHECK (length(name) BETWEEN 1 AND 100),
    settings    TEXT    NOT NULL DEFAULT '{}' CHECK (json_valid(settings)),
    created_at  INTEGER NOT NULL,
    CHECK ((org_id IS NULL) <> (owner_user IS NULL))
) STRICT;
CREATE UNIQUE INDEX _forgebase_projects_org_slug ON _forgebase_projects (org_id, slug) WHERE org_id IS NOT NULL;
CREATE UNIQUE INDEX _forgebase_projects_user_slug ON _forgebase_projects (owner_user, slug) WHERE owner_user IS NOT NULL;

-- The tenants of the data core.
CREATE TABLE _forgebase_environments (
    id          TEXT    NOT NULL PRIMARY KEY CHECK (length(id) BETWEEN 1 AND 64),
    project_id  TEXT    NOT NULL REFERENCES _forgebase_projects(id) ON DELETE CASCADE,
    name        TEXT    NOT NULL CHECK (length(name) BETWEEN 1 AND 32),
    created_at  INTEGER NOT NULL,
    UNIQUE (project_id, name)
) STRICT;

-- Roles defined at realm scope (org_id NULL, usable in every org of the realm
-- and in realm bindings) or at org scope. `system` roles are seeded and cannot
-- be deleted; `assignable = 0` marks the platform super-admin role, which is
-- documentation only: it can never be granted through a membership.
CREATE TABLE _forgebase_roles (
    id          TEXT    NOT NULL PRIMARY KEY,
    realm_id    TEXT    NOT NULL REFERENCES _forgebase_realms(id) ON DELETE CASCADE,
    org_id      TEXT    REFERENCES _forgebase_orgs(id) ON DELETE CASCADE,
    key         TEXT    NOT NULL CHECK (length(key) BETWEEN 1 AND 64),
    name        TEXT    NOT NULL,
    description TEXT    NOT NULL DEFAULT '',
    allow       TEXT    NOT NULL DEFAULT '[]' CHECK (json_valid(allow)),
    deny        TEXT    NOT NULL DEFAULT '[]' CHECK (json_valid(deny)),
    system      INTEGER NOT NULL DEFAULT 0 CHECK (system IN (0, 1)),
    assignable  INTEGER NOT NULL DEFAULT 1 CHECK (assignable IN (0, 1)),
    created_at  INTEGER NOT NULL
) STRICT;
CREATE UNIQUE INDEX _forgebase_roles_key ON _forgebase_roles (realm_id, coalesce(org_id, ''), key);

INSERT INTO _forgebase_roles (id, realm_id, org_id, key, name, description, allow, deny, system, assignable, created_at) VALUES
 ('role_super_admin', 'default', NULL, 'super_admin', 'Super Admin', 'Platform capability of ForgeBase administrators; never an org role', '["*"]', '[]', 1, 0, unixepoch()),
 ('role_owner', 'default', NULL, 'owner', 'Owner', 'Full organization control', '["*"]', '[]', 1, 1, unixepoch()),
 ('role_admin', 'default', NULL, 'admin', 'Admin', 'Manage members, settings and resources',
  '["org:read","org:update","users:*","teams:*","projects:*","files:*","api_keys:*","analytics:*","ai:*","billing:read","roles:*","acl:*","data:*","engine:*"]',
  '["org:delete"]', 1, 1, unixepoch()),
 ('role_manager', 'default', NULL, 'manager', 'Manager', 'Manage assigned teams and projects',
  '["org:read","users:read","teams:read","teams:update","teams:members:manage","projects:read","projects:create","projects:update","projects:share","files:*","analytics:read","data:*:read","data:*:write","engine:*"]',
  '[]', 1, 1, unixepoch()),
 ('role_developer', 'default', NULL, 'developer', 'Developer', 'API, integrations and development',
  '["org:read","users:read","teams:read","projects:read","projects:update","api_keys:*","files:read","files:upload","files:download","ai:generate","data:*","engine:*"]',
  '[]', 1, 1, unixepoch()),
 ('role_billing_manager', 'default', NULL, 'billing_manager', 'Billing Manager', 'Plans, invoices and payments',
  '["org:read","users:read","billing:*"]', '[]', 1, 1, unixepoch()),
 ('role_analyst', 'default', NULL, 'analyst', 'Analyst', 'Read analytics and reports',
  '["org:read","users:read","projects:read","analytics:*","data:*:read","engine:kv_read","engine:graph_read","engine:read_jobs"]', '[]', 1, 1, unixepoch()),
 ('role_member', 'default', NULL, 'member', 'Member', 'Standard resource access',
  '["org:read","users:read","teams:read","projects:read","projects:create","files:read","files:upload","files:download","ai:generate","data:*:read","data:*:create","engine:kv_read","engine:kv_write","engine:graph_read","engine:graph_write","engine:read_jobs","engine:enqueue_job"]',
  '[]', 1, 1, unixepoch()),
 ('role_viewer', 'default', NULL, 'viewer', 'Viewer', 'Read-only',
  '["org:read","users:read","teams:read","projects:read","files:read","files:download","analytics:read","data:*:read","engine:kv_read","engine:graph_read","engine:read_jobs"]',
  '[]', 1, 1, unixepoch()),
 ('role_guest', 'default', NULL, 'guest', 'Guest', 'Limited shared-resource access (resource ACL grants only)',
  '["org:read"]', '[]', 1, 1, unixepoch());

-- Named bundles of roles.
CREATE TABLE _forgebase_role_sets (
    id          TEXT    NOT NULL PRIMARY KEY,
    realm_id    TEXT    NOT NULL REFERENCES _forgebase_realms(id) ON DELETE CASCADE,
    org_id      TEXT    REFERENCES _forgebase_orgs(id) ON DELETE CASCADE,
    key         TEXT    NOT NULL CHECK (length(key) BETWEEN 1 AND 64),
    name        TEXT    NOT NULL,
    roles       TEXT    NOT NULL DEFAULT '[]' CHECK (json_valid(roles)),
    created_at  INTEGER NOT NULL
) STRICT;
CREATE UNIQUE INDEX _forgebase_role_sets_key ON _forgebase_role_sets (realm_id, coalesce(org_id, ''), key);

-- Realm-level role bindings (realm RBAC, outside any organization).
CREATE TABLE _forgebase_realm_bindings (
    id          TEXT    NOT NULL PRIMARY KEY,
    realm_id    TEXT    NOT NULL REFERENCES _forgebase_realms(id) ON DELETE CASCADE,
    principal   TEXT    NOT NULL,
    roles       TEXT    NOT NULL DEFAULT '[]' CHECK (json_valid(roles)),
    role_sets   TEXT    NOT NULL DEFAULT '[]' CHECK (json_valid(role_sets)),
    created_at  INTEGER NOT NULL,
    UNIQUE (realm_id, principal)
) STRICT;

CREATE TABLE _forgebase_memberships (
    id          TEXT    NOT NULL PRIMARY KEY,
    org_id      TEXT    NOT NULL REFERENCES _forgebase_orgs(id) ON DELETE CASCADE,
    principal   TEXT    NOT NULL,
    roles       TEXT    NOT NULL DEFAULT '[]' CHECK (json_valid(roles)),
    role_sets   TEXT    NOT NULL DEFAULT '[]' CHECK (json_valid(role_sets)),
    status      TEXT    NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'suspended')),
    created_at  INTEGER NOT NULL,
    UNIQUE (org_id, principal)
) STRICT;
CREATE INDEX _forgebase_memberships_principal ON _forgebase_memberships (principal);

-- Teams form a tree inside one organization. Roles and denies of a team are
-- inherited by its descendant teams' members.
CREATE TABLE _forgebase_teams (
    id          TEXT    NOT NULL PRIMARY KEY,
    org_id      TEXT    NOT NULL REFERENCES _forgebase_orgs(id) ON DELETE CASCADE,
    parent_id   TEXT    REFERENCES _forgebase_teams(id) ON DELETE CASCADE,
    key         TEXT    NOT NULL CHECK (length(key) BETWEEN 1 AND 64),
    name        TEXT    NOT NULL,
    roles       TEXT    NOT NULL DEFAULT '[]' CHECK (json_valid(roles)),
    deny        TEXT    NOT NULL DEFAULT '[]' CHECK (json_valid(deny)),
    created_at  INTEGER NOT NULL,
    UNIQUE (org_id, key)
) STRICT;

CREATE TABLE _forgebase_team_members (
    team_id     TEXT    NOT NULL REFERENCES _forgebase_teams(id) ON DELETE CASCADE,
    principal   TEXT    NOT NULL,
    created_at  INTEGER NOT NULL,
    PRIMARY KEY (team_id, principal)
) STRICT;

-- Explicit per-principal allow/deny (user overrides). org_id NULL: realm-wide.
CREATE TABLE _forgebase_permission_overrides (
    id          TEXT    NOT NULL PRIMARY KEY,
    realm_id    TEXT    NOT NULL REFERENCES _forgebase_realms(id) ON DELETE CASCADE,
    org_id      TEXT    REFERENCES _forgebase_orgs(id) ON DELETE CASCADE,
    principal   TEXT    NOT NULL,
    permission  TEXT    NOT NULL,
    effect      TEXT    NOT NULL CHECK (effect IN ('allow', 'deny')),
    created_at  INTEGER NOT NULL
) STRICT;

-- Resource-level ACL (`resource_permissions`): grants or denies one
-- permission on one resource (`project:<id>`, `environment:<id>`,
-- `record:<collection>/<id>`, `file:<id>`, ...) to a principal, a team
-- (`team:<id>`) or everyone holding a role in the org (`role:<key>`).
CREATE TABLE _forgebase_resource_permissions (
    id            TEXT    NOT NULL PRIMARY KEY,
    realm_id      TEXT    NOT NULL REFERENCES _forgebase_realms(id) ON DELETE CASCADE,
    resource_type TEXT    NOT NULL CHECK (length(resource_type) BETWEEN 1 AND 32),
    resource_id   TEXT    NOT NULL CHECK (length(resource_id) BETWEEN 1 AND 200),
    grantee       TEXT    NOT NULL,
    permission    TEXT    NOT NULL,
    effect        TEXT    NOT NULL CHECK (effect IN ('allow', 'deny')),
    created_by    TEXT    NOT NULL,
    created_at    INTEGER NOT NULL
) STRICT;
CREATE INDEX _forgebase_resource_permissions_res ON _forgebase_resource_permissions (resource_type, resource_id);

-- Account suspension: a suspended principal is denied everywhere (step 1).
CREATE TABLE _forgebase_suspensions (
    principal   TEXT    NOT NULL PRIMARY KEY,
    reason      TEXT    NOT NULL DEFAULT '',
    suspended_by TEXT   NOT NULL,
    suspended_at INTEGER NOT NULL,
    until_at    INTEGER
) STRICT;

-- Collection rules (PocketBase-style). NULL rule = locked (privileged only),
-- '' = everyone, otherwise a rule expression. `fields`: {field: {read, write}}.
CREATE TABLE _forgebase_collection_policies (
    collection_id TEXT    NOT NULL PRIMARY KEY,
    list_rule     TEXT,
    view_rule     TEXT,
    create_rule   TEXT,
    update_rule   TEXT,
    delete_rule   TEXT,
    fields        TEXT    NOT NULL DEFAULT '{}' CHECK (json_valid(fields)),
    updated_by    TEXT    NOT NULL,
    updated_at    INTEGER NOT NULL
) STRICT;

CREATE TABLE _forgebase_invitations (
    id          TEXT    NOT NULL PRIMARY KEY,
    org_id      TEXT    NOT NULL REFERENCES _forgebase_orgs(id) ON DELETE CASCADE,
    email       TEXT    NOT NULL COLLATE NOCASE,
    roles       TEXT    NOT NULL DEFAULT '[]' CHECK (json_valid(roles)),
    token_hash  TEXT    NOT NULL UNIQUE,
    invited_by  TEXT    NOT NULL,
    created_at  INTEGER NOT NULL,
    expires_at  INTEGER NOT NULL,
    accepted_at INTEGER,
    accepted_by TEXT,
    revoked_at  INTEGER
) STRICT;

-- Denied decisions with the pipeline step that failed (append-only).
CREATE TABLE _forgebase_authz_audit (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    at          INTEGER NOT NULL,
    realm_id    TEXT    NOT NULL,
    principal   TEXT    NOT NULL,
    tenant      TEXT,
    org_id      TEXT,
    permission  TEXT    NOT NULL,
    resource    TEXT,
    step        TEXT    NOT NULL CHECK (step IN ('authentication', 'tenant', 'permission', 'entitlement', 'resource', 'quota')),
    reason      TEXT    NOT NULL,
    request_id  TEXT
) STRICT;
CREATE INDEX _forgebase_authz_audit_at ON _forgebase_authz_audit (at);
