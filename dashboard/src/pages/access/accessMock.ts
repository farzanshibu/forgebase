/** In-memory fake of the FB-21 policy REST (`/api/v1` tenancy + authorization) for component tests. */
import type { AclEntry, Invitation, Member, Org, Override, Policy, PrincipalSuspension, Project, Realm, Role, RoleSet, Team } from "../../api";
import { envelope, type MockApi, type MockReply, type MockRequest } from "../../test/mockApi";

export interface AccessState {
  superAdmin: boolean;
  realms: Realm[];
  orgs: Org[];
  projects: (Project & { environments: { id: string; project_id: string; name: string }[] })[];
  members: Member[];
  teams: Team[];
  roles: Role[];
  roleSets: RoleSet[];
  bindings: { principal: string; roles: string[]; role_sets: string[] }[];
  overrides: Override[];
  acl: AclEntry[];
  suspensions: PrincipalSuspension[];
  invitations: Invitation[];
  policies: Record<string, Policy>;
  /** When set, `PUT` role answers 403 with this message (escalation guard). */
  rolePutForbidden?: string;
}

let seq = 1;
const id = (p: string) => `${p}${seq++}`;
const role = (key: string, over: Partial<Role> = {}): Role => ({
  id: `r_${key}`, realm_id: "default", org_id: null, key, name: key, description: "", allow: [], deny: [], system: false, assignable: true, ...over,
});

export function seedAccess(): AccessState {
  return {
    superAdmin: true,
    realms: [{ id: "default", name: "Default" }],
    orgs: [{ id: "org_acme", realm_id: "default", slug: "acme", name: "Acme Inc" }],
    projects: [{ id: "prj_web", realm_id: "default", org_id: "org_acme", owner_user: null, slug: "web", name: "Web", environments: [{ id: "env_dev", project_id: "prj_web", name: "development" }] }],
    members: [{ id: "m1", scope_id: "org_acme", principal: "user:users/alice", roles: ["owner"], role_sets: [], status: "active" }],
    teams: [
      { id: "t_eng", org_id: "org_acme", parent_id: null, key: "eng", name: "Engineering", roles: ["developer"], deny: [], members: ["user:users/alice"] },
      { id: "t_be", org_id: "org_acme", parent_id: "t_eng", key: "backend", name: "Backend", roles: [], deny: ["billing:manage"], members: [] },
    ],
    roles: [
      role("owner", { system: true, allow: ["*"] }),
      role("super_admin", { system: true, assignable: false, allow: ["*"] }),
      role("support", { org_id: "org_acme", allow: ["users:read", "data:posts:*"], deny: ["billing:manage"] }),
    ],
    roleSets: [{ id: "rs1", realm_id: "default", org_id: "org_acme", key: "staff", name: "Staff", roles: ["support"] }],
    bindings: [{ principal: "user:users/root", roles: ["owner"], role_sets: [] }],
    overrides: [
      { id: "ov1", realm_id: "default", org_id: "org_acme", principal: "user:users/bob", permission: "projects:delete", effect: "deny" },
      { id: "ov2", realm_id: "default", org_id: "org_acme", principal: "user:users/carol", permission: "projects:read", effect: "allow" },
    ],
    acl: [
      { id: "acl1", realm_id: "default", resource_type: "project", resource_id: "prj_web", grantee: "user:users/dave", permission: "projects:update", effect: "deny" },
      { id: "acl2", realm_id: "default", resource_type: "project", resource_id: "prj_web", grantee: "user:users/erin", permission: "projects:read", effect: "allow" },
    ],
    suspensions: [{ principal: "user:users/mallory", reason: "abuse", until: null }],
    invitations: [{ id: "inv1", email: "new@acme.test", roles: ["support"], status: "pending", expires_at: "2026-11-16T00:00:00Z" }],
    policies: {
      posts: { collection: "posts", list_rule: "", view_rule: "", create_rule: '@request.auth.id != ""', update_rule: null, delete_rule: null, fields: { secret: { read: '@request.auth.type = "admin"', write: null } } },
    },
  };
}

const ok = (body?: unknown, status = 200): MockReply => ({ status, body });
const nf = (what: string) => envelope(404, "not_found", `${what} not found`);

/** Registers every policy route on `mock` and returns the state the handlers mutate. */
export function registerAccess(mock: MockApi, state: AccessState = seedAccess()): AccessState {
  const s = state;
  const org = (r: MockRequest) => s.orgs.find((o) => o.id === r.params.org || o.slug === r.params.org);
  const body = <T,>(r: MockRequest) => r.body as T;
  const rolesOf = (o?: string) => s.roles.filter((r) => r.org_id === null || r.org_id === (o ?? null));
  const setsOf = (o?: string) => s.roleSets.filter((r) => r.org_id === (o ?? null));

  mock.on("GET", "/api/v1/authz/me", (r) => ok({ principal: "admin:root", realm_id: "default", org_id: r.query.get("org"), super_admin: s.superAdmin, suspended: false, roles: [], allow: [], deny: [], teams: [] }));
  mock.on("GET", "/api/v1/realms", () => ok(s.realms));
  mock.on("POST", "/api/v1/realms", (r) => { const b = body<{ slug: string; name: string }>(r); const x = { id: b.slug, name: b.name }; s.realms.push(x); return ok(x, 201); });
  mock.on("GET", "/api/v1/orgs", () => ok(s.orgs));
  mock.on("POST", "/api/v1/orgs", (r) => { const b = body<{ slug: string; name: string }>(r); const x: Org = { id: id("org_"), realm_id: "default", slug: b.slug, name: b.name }; s.orgs.push(x); return ok(x, 201); });
  mock.on("GET", "/api/v1/orgs/{org}", (r) => { const o = org(r); return o ? ok(o) : nf("organization"); });
  mock.on("PATCH", "/api/v1/orgs/{org}", (r) => { const o = org(r); if (!o) return nf("organization"); o.name = body<{ name: string }>(r).name; return ok(o); });
  mock.on("DELETE", "/api/v1/orgs/{org}", (r) => { s.orgs = s.orgs.filter((o) => o !== org(r)); return ok(undefined, 204); });

  mock.on("GET", "/api/v1/orgs/{org}/projects", (r) => ok(s.projects.filter((p) => p.org_id === org(r)?.id).map((p) => ({ ...p, environments: undefined }))));
  mock.on("POST", "/api/v1/orgs/{org}/projects", (r) => {
    const b = body<{ slug: string; name: string }>(r); const pid = id("prj_");
    const p = { id: pid, realm_id: "default", org_id: org(r)!.id, owner_user: null, slug: b.slug, name: b.name, environments: ["development", "staging", "production"].map((n) => ({ id: id("env_"), project_id: pid, name: n })) };
    s.projects.push(p); return ok(p, 201);
  });
  mock.on("GET", "/api/v1/projects/{project}", (r) => { const p = s.projects.find((x) => x.id === r.params.project); return p ? ok(p) : nf("project"); });
  mock.on("DELETE", "/api/v1/projects/{project}", (r) => { s.projects = s.projects.filter((x) => x.id !== r.params.project); return ok(undefined, 204); });
  mock.on("POST", "/api/v1/projects/{project}/environments", (r) => {
    const p = s.projects.find((x) => x.id === r.params.project); if (!p) return nf("project");
    const e = { id: id("env_"), project_id: p.id, name: body<{ name: string }>(r).name }; p.environments.push(e); return ok(e, 201);
  });

  mock.on("GET", "/api/v1/orgs/{org}/members", (r) => ok(s.members.filter((m) => m.scope_id === org(r)?.id)));
  mock.on("PUT", "/api/v1/orgs/{org}/members", (r) => {
    const b = body<{ principal: string; roles: string[]; role_sets: string[] }>(r); const o = org(r)!;
    const cur = s.members.find((m) => m.scope_id === o.id && m.principal === b.principal);
    if (cur) Object.assign(cur, { roles: b.roles, role_sets: b.role_sets });
    else s.members.push({ id: id("m"), scope_id: o.id, principal: b.principal, roles: b.roles, role_sets: b.role_sets, status: "active" });
    return ok({ org_id: o.id, principal: b.principal, roles: b.roles, role_sets: b.role_sets, status: "active" });
  });
  mock.on("POST", "/api/v1/orgs/{org}/members/remove", (r) => { const p = body<{ principal: string }>(r).principal; s.members = s.members.filter((m) => m.principal !== p); return ok(undefined, 204); });
  mock.on("POST", "/api/v1/orgs/{org}/members/status", (r) => {
    const b = body<{ principal: string; active: boolean }>(r);
    const m = s.members.find((x) => x.principal === b.principal); if (m) m.status = b.active ? "active" : "suspended"; return ok(undefined, 204);
  });

  mock.on("GET", "/api/v1/orgs/{org}/teams", (r) => ok(s.teams.filter((t) => t.org_id === org(r)?.id)));
  mock.on("POST", "/api/v1/orgs/{org}/teams", (r) => {
    const b = body<{ key: string; name: string; parent_id?: string | null; roles: string[]; deny: string[] }>(r);
    const t: Team = { id: id("t_"), org_id: org(r)!.id, parent_id: b.parent_id ?? null, key: b.key, name: b.name, roles: b.roles, deny: b.deny, members: [] };
    s.teams.push(t); return ok(t, 201);
  });
  mock.on("DELETE", "/api/v1/teams/{team}", (r) => {
    const drop = new Set([r.params.team]);
    for (let grew = true; grew;) { grew = false; for (const t of s.teams) if (t.parent_id && drop.has(t.parent_id) && !drop.has(t.id)) { drop.add(t.id); grew = true; } }
    s.teams = s.teams.filter((t) => !drop.has(t.id)); return ok(undefined, 204);
  });
  mock.on("POST", "/api/v1/teams/{team}/members", (r) => { s.teams.find((t) => t.id === r.params.team)?.members.push(body<{ principal: string }>(r).principal); return ok(undefined, 204); });
  mock.on("POST", "/api/v1/teams/{team}/members/remove", (r) => {
    const t = s.teams.find((x) => x.id === r.params.team); if (t) t.members = t.members.filter((m) => m !== body<{ principal: string }>(r).principal); return ok(undefined, 204);
  });

  const putRole = (o: string | undefined, r: MockRequest): MockReply => {
    if (s.rolePutForbidden) return envelope(403, "forbidden", s.rolePutForbidden, { step: "permission" });
    const b = body<{ key: string; name?: string; description?: string; allow: string[]; deny: string[] }>(r);
    const cur = s.roles.find((x) => x.key === b.key && x.org_id === (o ?? null));
    const next = role(b.key, { org_id: o ?? null, name: b.name ?? b.key, description: b.description ?? "", allow: b.allow, deny: b.deny });
    if (cur) Object.assign(cur, { name: next.name, description: next.description, allow: next.allow, deny: next.deny }); else s.roles.push(next);
    return ok(cur ?? next);
  };
  mock.on("GET", "/api/v1/orgs/{org}/roles", (r) => ok(rolesOf(org(r)?.id)));
  mock.on("PUT", "/api/v1/orgs/{org}/roles", (r) => putRole(org(r)?.id, r));
  mock.on("DELETE", "/api/v1/orgs/{org}/roles/{key}", (r) => { s.roles = s.roles.filter((x) => !(x.key === r.params.key && x.org_id === org(r)?.id)); return ok(undefined, 204); });
  mock.on("GET", "/api/v1/roles", () => ok(rolesOf()));
  mock.on("PUT", "/api/v1/roles", (r) => putRole(undefined, r));
  mock.on("DELETE", "/api/v1/roles/{key}", (r) => { s.roles = s.roles.filter((x) => !(x.key === r.params.key && x.org_id === null)); return ok(undefined, 204); });

  const putSet = (o: string | undefined, r: MockRequest): MockReply => {
    const b = body<{ key: string; name: string; roles: string[] }>(r);
    const cur = s.roleSets.find((x) => x.key === b.key && x.org_id === (o ?? null));
    if (cur) Object.assign(cur, { name: b.name, roles: b.roles });
    const made = cur ?? { id: id("rs"), realm_id: "default", org_id: o ?? null, key: b.key, name: b.name, roles: b.roles };
    if (!cur) s.roleSets.push(made);
    return ok(made);
  };
  mock.on("GET", "/api/v1/orgs/{org}/role-sets", (r) => ok(setsOf(org(r)?.id)));
  mock.on("PUT", "/api/v1/orgs/{org}/role-sets", (r) => putSet(org(r)?.id, r));
  mock.on("GET", "/api/v1/role-sets", () => ok(setsOf()));
  mock.on("PUT", "/api/v1/role-sets", (r) => putSet(undefined, r));

  mock.on("GET", "/api/v1/realm-bindings", () => ok(s.bindings));
  mock.on("PUT", "/api/v1/realm-bindings", (r) => {
    const b = body<{ principal: string; roles: string[]; role_sets: string[] }>(r);
    const cur = s.bindings.find((x) => x.principal === b.principal); if (cur) Object.assign(cur, b); else s.bindings.push(b); return ok(b);
  });

  mock.on("GET", "/api/v1/overrides", (r) => ok(s.overrides.filter((o) => !r.query.get("org") || o.org_id === org({ ...r, params: { org: r.query.get("org")! } })?.id)));
  mock.on("POST", "/api/v1/overrides", (r) => { const o = { id: id("ov"), realm_id: "default", ...body<Omit<Override, "id" | "realm_id">>(r) }; s.overrides.push(o); return ok(o, 201); });
  mock.on("DELETE", "/api/v1/overrides/{id}", (r) => { s.overrides = s.overrides.filter((o) => o.id !== r.params.id); return ok(undefined, 204); });

  mock.on("GET", "/api/v1/acl", (r) => ok(s.acl.filter((a) => a.resource_type === r.query.get("resource_type") && a.resource_id === r.query.get("resource_id"))));
  mock.on("POST", "/api/v1/acl", (r) => { const a = { id: id("acl"), realm_id: "default", ...body<Omit<AclEntry, "id" | "realm_id">>(r) }; s.acl.push(a); return ok(a, 201); });
  mock.on("DELETE", "/api/v1/acl/{id}", (r) => { s.acl = s.acl.filter((a) => a.id !== r.params.id); return ok(undefined, 204); });

  mock.on("GET", "/api/v1/suspensions", () => ok(s.suspensions));
  mock.on("POST", "/api/v1/suspensions", (r) => { s.suspensions.push(body<PrincipalSuspension>(r)); return ok(undefined, 204); });
  mock.on("POST", "/api/v1/suspensions/lift", (r) => { s.suspensions = s.suspensions.filter((x) => x.principal !== body<{ principal: string }>(r).principal); return ok(undefined, 204); });

  mock.on("GET", "/api/v1/policies/{collection}", (r) => ok(s.policies[r.params.collection!] ?? { collection: r.params.collection, list_rule: null, view_rule: null, create_rule: null, update_rule: null, delete_rule: null, fields: {} }));
  mock.on("PUT", "/api/v1/policies/{collection}", (r) => {
    const b = body<Omit<Policy, "collection">>(r);
    for (const k of ["list_rule", "view_rule", "create_rule", "update_rule", "delete_rule"] as const)
      if (b[k]?.includes("nosuchfield")) return envelope(400, "validation_failed", "invalid rule", { fields: { [k]: { code: "unknown_field", message: "the rule names a field that does not exist: nosuchfield" } } });
    const p = { collection: r.params.collection!, ...b }; s.policies[p.collection] = p; return ok(p);
  });
  mock.on("DELETE", "/api/v1/policies/{collection}", (r) => { delete s.policies[r.params.collection!]; return ok(undefined, 204); });

  mock.on("GET", "/api/v1/orgs/{org}/invitations", (r) => ok(org(r) ? s.invitations : []));
  mock.on("POST", "/api/v1/orgs/{org}/invitations", (r) => {
    const b = body<{ email: string; roles: string[] }>(r);
    const inv = { id: id("inv"), email: b.email, roles: b.roles, status: "pending", expires_at: "2026-11-16T00:00:00Z" };
    s.invitations.push(inv); return ok({ ...inv, token: "tok_one_time_secret" }, 201);
  });

  mock.on("POST", "/api/v1/authz/check", (r) => {
    const b = body<{ permission: string; principal?: string }>(r);
    return ok(b.permission.includes("denied") ? { allowed: false, step: "permission", reason: `no grant for ${b.permission}` } : { allowed: true });
  });
  return s;
}
