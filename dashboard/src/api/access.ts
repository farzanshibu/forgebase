/** Typed client for the FB-21 policy REST (`/api/v1` tenancy + authorization), ADR-0014 section 3. */
import { request, send } from "./client";

const V1 = "/api/v1";
const enc = encodeURIComponent;
const list = <T>(path: string, signal?: AbortSignal) => request<T[]>(`${V1}${path}`, { signal });

export type Effect = "allow" | "deny";
export interface Realm { id: string; name: string }
export interface Org { id: string; realm_id: string; slug: string; name: string }
export interface Environment { id: string; project_id: string; name: string }
export interface Project { id: string; realm_id: string; org_id: string | null; owner_user: string | null; slug: string; name: string; environments?: Environment[] }
export interface Role {
  id: string; realm_id: string; org_id: string | null; key: string; name: string; description: string;
  allow: string[]; deny: string[]; system: boolean; assignable: boolean;
}
export interface RoleSet { id: string; realm_id: string; org_id: string | null; key: string; name: string; roles: string[] }
export interface Member { id: string; scope_id: string; principal: string; roles: string[]; role_sets: string[]; status: string }
export interface Team { id: string; org_id: string; parent_id: string | null; key: string; name: string; roles: string[]; deny: string[]; members: string[] }
export interface Override { id: string; realm_id: string; org_id: string | null; principal: string; permission: string; effect: Effect }
export interface AclEntry { id: string; realm_id: string; resource_type: string; resource_id: string; grantee: string; permission: string; effect: Effect }
export interface PrincipalSuspension { principal: string; reason: string; until?: number | string | null; created_at?: string; created_by?: string }
export interface Invitation { id: string; org_id?: string; email: string; roles: string[]; status?: string; expires_at: number | string; token?: string }
export interface FieldRules { read?: string | null; write?: string | null }
export interface Policy {
  collection: string;
  list_rule: string | null; view_rule: string | null; create_rule: string | null; update_rule: string | null; delete_rule: string | null;
  fields: Record<string, FieldRules>;
}
export type PolicyInput = Omit<Policy, "collection">;
export interface CheckBody { permission: string; org?: string; resource_type?: string; resource_id?: string; principal?: string }
export interface CheckResult { allowed: boolean; step?: string; reason?: string }
export interface RoleInput { key: string; name?: string; description?: string; allow: string[]; deny: string[] }

/** Every `method path` the client calls (documented in the OpenAPI document once it is refreshed). */
export const ACCESS_OPERATIONS = [
  "get /api/v1/realms", "post /api/v1/realms",
  "get /api/v1/orgs", "post /api/v1/orgs", "get /api/v1/orgs/{org}", "patch /api/v1/orgs/{org}", "delete /api/v1/orgs/{org}",
  "get /api/v1/orgs/{org}/projects", "post /api/v1/orgs/{org}/projects", "get /api/v1/projects/{project}", "delete /api/v1/projects/{project}",
  "post /api/v1/projects/{project}/environments",
  "get /api/v1/orgs/{org}/members", "put /api/v1/orgs/{org}/members", "post /api/v1/orgs/{org}/members/remove", "post /api/v1/orgs/{org}/members/status",
  "get /api/v1/orgs/{org}/teams", "post /api/v1/orgs/{org}/teams", "delete /api/v1/teams/{team}",
  "post /api/v1/teams/{team}/members", "post /api/v1/teams/{team}/members/remove",
  "get /api/v1/orgs/{org}/roles", "put /api/v1/orgs/{org}/roles", "delete /api/v1/orgs/{org}/roles/{key}",
  "get /api/v1/orgs/{org}/role-sets", "put /api/v1/orgs/{org}/role-sets",
  "get /api/v1/roles", "put /api/v1/roles", "delete /api/v1/roles/{key}", "get /api/v1/role-sets", "put /api/v1/role-sets",
  "get /api/v1/realm-bindings", "put /api/v1/realm-bindings",
  "get /api/v1/overrides", "post /api/v1/overrides", "delete /api/v1/overrides/{id}",
  "get /api/v1/acl", "post /api/v1/acl", "delete /api/v1/acl/{id}",
  "get /api/v1/suspensions", "post /api/v1/suspensions", "post /api/v1/suspensions/lift",
  "get /api/v1/policies/{collection}", "put /api/v1/policies/{collection}", "delete /api/v1/policies/{collection}",
  "get /api/v1/orgs/{org}/invitations", "post /api/v1/orgs/{org}/invitations",
  "post /api/v1/authz/check", "get /api/v1/authz/me",
] as const;

export const listRealms = (s?: AbortSignal) => list<Realm>("/realms", s);
export const createRealm = (slug: string, name: string) => send<Realm>("POST", `${V1}/realms`, { slug, name });

export const listOrgs = (s?: AbortSignal) => list<Org>("/orgs", s);
export const createOrg = (slug: string, name: string) => send<Org>("POST", `${V1}/orgs`, { slug, name });
export const getOrg = (org: string, s?: AbortSignal) => request<Org>(`${V1}/orgs/${enc(org)}`, { signal: s });
export const renameOrg = (org: string, name: string) => send<Org>("PATCH", `${V1}/orgs/${enc(org)}`, { name });
export const deleteOrg = (org: string) => send<void>("DELETE", `${V1}/orgs/${enc(org)}`);

export const listProjects = (org: string, s?: AbortSignal) => list<Project>(`/orgs/${enc(org)}/projects`, s);
export const createProject = (org: string, slug: string, name: string) => send<Project>("POST", `${V1}/orgs/${enc(org)}/projects`, { slug, name });
export const getProject = (project: string, s?: AbortSignal) => request<Project>(`${V1}/projects/${enc(project)}`, { signal: s });
export const deleteProject = (project: string) => send<void>("DELETE", `${V1}/projects/${enc(project)}`);
export const createEnvironment = (project: string, name: string) => send<Environment>("POST", `${V1}/projects/${enc(project)}/environments`, { name });

export const listMembers = (org: string, s?: AbortSignal) => list<Member>(`/orgs/${enc(org)}/members`, s);
export const putMember = (org: string, principal: string, roles: string[], role_sets: string[]) =>
  send<Member>("PUT", `${V1}/orgs/${enc(org)}/members`, { principal, roles, role_sets });
export const removeMember = (org: string, principal: string) => send<void>("POST", `${V1}/orgs/${enc(org)}/members/remove`, { principal });
export const setMemberStatus = (org: string, principal: string, active: boolean) =>
  send<void>("POST", `${V1}/orgs/${enc(org)}/members/status`, { principal, active });

export const listTeams = (org: string, s?: AbortSignal) => list<Team>(`/orgs/${enc(org)}/teams`, s);
export const createTeam = (org: string, t: { key: string; name: string; parent_id?: string | null; roles?: string[]; deny?: string[] }) =>
  send<Team>("POST", `${V1}/orgs/${enc(org)}/teams`, { roles: [], deny: [], ...t });
export const deleteTeam = (team: string) => send<void>("DELETE", `${V1}/teams/${enc(team)}`);
export const addTeamMember = (team: string, principal: string) => send<void>("POST", `${V1}/teams/${enc(team)}/members`, { principal });
export const removeTeamMember = (team: string, principal: string) => send<void>("POST", `${V1}/teams/${enc(team)}/members/remove`, { principal });

const rolesPath = (org?: string) => (org ? `/orgs/${enc(org)}/roles` : "/roles");
const setsPath = (org?: string) => (org ? `/orgs/${enc(org)}/role-sets` : "/role-sets");
export const listRoles = (org?: string, s?: AbortSignal) => list<Role>(rolesPath(org), s);
export const putRole = (org: string | undefined, r: RoleInput) => send<Role>("PUT", `${V1}${rolesPath(org)}`, r);
export const deleteRole = (org: string | undefined, key: string) => send<void>("DELETE", `${V1}${rolesPath(org)}/${enc(key)}`);
export const listRoleSets = (org?: string, s?: AbortSignal) => list<RoleSet>(setsPath(org), s);
export const putRoleSet = (org: string | undefined, key: string, name: string, roles: string[]) =>
  send<RoleSet>("PUT", `${V1}${setsPath(org)}`, { key, name, roles });

export const listRealmBindings = (s?: AbortSignal) => list<Omit<Member, "id" | "scope_id" | "status"> & { id?: string }>("/realm-bindings", s);
export const putRealmBinding = (principal: string, roles: string[], role_sets: string[]) =>
  send<unknown>("PUT", `${V1}/realm-bindings`, { principal, roles, role_sets });

export const listOverrides = (org?: string, s?: AbortSignal) => list<Override>(`/overrides${org ? `?org=${enc(org)}` : ""}`, s);
export const addOverride = (o: { org_id?: string | null; principal: string; permission: string; effect: Effect }) => send<Override>("POST", `${V1}/overrides`, o);
export const deleteOverride = (id: string) => send<void>("DELETE", `${V1}/overrides/${enc(id)}`);

export const listAcl = (resource_type: string, resource_id: string, s?: AbortSignal) =>
  list<AclEntry>(`/acl?resource_type=${enc(resource_type)}&resource_id=${enc(resource_id)}`, s);
export const putAcl = (e: Omit<AclEntry, "id" | "realm_id">) => send<AclEntry>("POST", `${V1}/acl`, e);
export const deleteAcl = (id: string) => send<void>("DELETE", `${V1}/acl/${enc(id)}`);

export const listGlobalSuspensions = (s?: AbortSignal) => list<PrincipalSuspension>("/suspensions", s);
export const suspendPrincipal = (principal: string, reason: string, until?: number | null) => send<void>("POST", `${V1}/suspensions`, { principal, reason, until: until ?? null });
export const liftSuspension = (principal: string) => send<void>("POST", `${V1}/suspensions/lift`, { principal });

export const getPolicy = (collection: string, s?: AbortSignal) => request<Policy>(`${V1}/policies/${enc(collection)}`, { signal: s });
export const putPolicy = (collection: string, p: PolicyInput) => send<Policy>("PUT", `${V1}/policies/${enc(collection)}`, p);
export const deletePolicy = (collection: string) => send<void>("DELETE", `${V1}/policies/${enc(collection)}`);

export const listInvitations = (org: string, s?: AbortSignal) => list<Invitation>(`/orgs/${enc(org)}/invitations`, s);
export const invite = (org: string, email: string, roles: string[]) => send<Invitation>("POST", `${V1}/orgs/${enc(org)}/invitations`, { email, roles });

export const authzCheck = (b: CheckBody) => send<CheckResult>("POST", `${V1}/authz/check`, b);

export interface AuthzMe { principal: string; realm_id: string; org_id: string | null; super_admin: boolean; suspended: boolean; roles: string[]; allow: string[]; deny: string[]; teams: string[] }
export const authzMe = (org?: string, s?: AbortSignal) => request<AuthzMe>(`${V1}/authz/me${org ? `?org=${enc(org)}` : ""}`, { signal: s });
