import type { Transport } from "./http";

/*
 * Tenancy (`/api/v1`, FB-21): organizations, projects and environments, members, invitations, teams, roles and role sets,
 * realm bindings, account suspensions: `client.orgs`.
 * The OpenAPI document types these responses as plain objects, so the wire types are written by hand; they follow the
 * server's JSON exactly (snake_case). Lists are plain arrays.
 */

/** A realm (platform super-admins). */
export interface TenantRealm {
  id: string;
  name: string;
}

export interface Organization {
  id: string;
  realm_id: string;
  /** Unique per realm. */
  slug: string;
  name: string;
}

export interface ProjectEnvironment {
  /** The tenant id of the environment's data. */
  id: string;
  project_id: string;
  /** `development`, `staging`, `production`, ... */
  name: string;
}

export interface OrgProject {
  id: string;
  realm_id: string;
  /** Owning organization; `null` for a personal project. */
  org_id: string | null;
  /** Principal key of the owner of a personal project. */
  owner_user: string | null;
  slug: string;
  name: string;
  /** Present on `projects.get`. */
  environments?: ProjectEnvironment[];
}

/** A membership. `scope_id` is the organization id. */
export interface OrgMember {
  id: string;
  scope_id: string;
  /** `user:<collection>/<id>` or `service:<id>`. */
  principal: string;
  roles: string[];
  role_sets: string[];
  /** `active` or `suspended`. */
  status: string;
}

/** What `members.set` answers. */
export interface MemberAssignment {
  org_id: string;
  principal: string;
  roles: string[];
  role_sets: string[];
  status: string;
}

export interface OrgTeam {
  id: string;
  org_id: string;
  parent_id: string | null;
  /** Unique per organization. */
  key: string;
  name: string;
  /** Roles every member (and every member of a sub-team) holds. */
  roles: string[];
  /** Denied permission patterns, inherited the same way. */
  deny: string[];
  /** Principal keys. */
  members: string[];
}

export interface PolicyRole {
  id: string;
  realm_id: string;
  /** Owning organization; `null` for a realm role. */
  org_id: string | null;
  key: string;
  name: string;
  description: string;
  /** Granted permission patterns (`records:*`, `projects:read`). */
  allow: string[];
  /** Denied patterns; a deny wins. */
  deny: string[];
  /** Seeded roles are edited by super-admins only. */
  system: boolean;
  assignable: boolean;
}

export interface PolicyRoleSet {
  id: string;
  realm_id: string;
  org_id: string | null;
  key: string;
  name: string;
  /** Role keys. */
  roles: string[];
}

/** Roles bound to a principal at realm level. */
export interface RealmBinding {
  id?: string;
  scope_id?: string;
  principal: string;
  roles: string[];
  role_sets: string[];
}

export interface OrgInvitation {
  id: string;
  org_id: string;
  email: string;
  roles: string[];
  expires_at: number;
  /** `pending`, `accepted` or `expired`. Only the list route sets it. */
  status?: "pending" | "accepted" | "expired";
  invited_by?: string | null;
  created_at?: number;
  accepted_at?: number | null;
  accepted_by?: string | null;
}

/** A fresh invitation: `token` is shown once (the server also mails it when a mailer is configured). */
export interface InvitationCreated extends OrgInvitation {
  token: string;
}

export interface InvitationAccepted {
  org_id: string;
  principal: string;
  roles: string[];
}

export interface AccountSuspension {
  principal: string;
  reason: string | null;
  suspended_by: string | null;
  /** Unix seconds. */
  created_at: number | null;
  /** Unix seconds; `null` until lifted. */
  until: number | null;
}

export interface CreateOrgInput {
  /** Unique per realm. */
  slug: string;
  name: string;
}

export interface OrgRoleInput {
  key: string;
  /** Display name (default: the key). */
  name?: string;
  description?: string;
  allow?: string[];
  deny?: string[];
}

export interface OrgRoleSetInput {
  key: string;
  name: string;
  /** Role keys. */
  roles: string[];
}

export interface OrgMemberInput {
  /** `user:<collection>/<id>` or `service:<id>`. */
  principal: string;
  roles?: string[];
  roleSets?: string[];
}

export interface OrgTeamInput {
  key: string;
  name: string;
  parentId?: string;
  roles?: string[];
  deny?: string[];
}

export interface AccountSuspendOptions {
  reason?: string;
  /** End of the suspension (Unix seconds or a `Date`); omit to suspend until lifted. */
  until?: number | Date;
}

const seg = encodeURIComponent;

/** Drops `undefined` so unset fields stay off the wire. */
const compact = (o: Record<string, unknown>): Record<string, unknown> => Object.fromEntries(Object.entries(o).filter(([, v]) => v !== undefined));

/** Realms (`client.orgs.realms`). */
export class TenantRealmsApi {
  constructor(private readonly t: Transport) {}

  /** Every realm (platform super-admins). */
  list(): Promise<TenantRealm[]> {
    return this.t.request("/api/v1/realms");
  }

  /** Creates a realm (platform super-admins). */
  create(id: string, name: string): Promise<TenantRealm> {
    return this.t.request("/api/v1/realms", { method: "POST", body: { id, name } });
  }
}

/** Projects and environments (`client.orgs.projects`). */
export class OrgProjectsApi {
  constructor(private readonly t: Transport) {}

  /** Projects of an organization, or the caller's personal projects when `org` is omitted. */
  list(org?: string): Promise<OrgProject[]> {
    return this.t.request(org === undefined ? "/api/v1/projects" : `/api/v1/orgs/${seg(org)}/projects`);
  }

  /** Creates a project (with the default environments) in an organization, or in the caller's personal scope. */
  create(input: CreateOrgInput, org?: string): Promise<OrgProject> {
    return this.t.request(org === undefined ? "/api/v1/projects" : `/api/v1/orgs/${seg(org)}/projects`, { method: "POST", body: { slug: input.slug, name: input.name } });
  }

  /** One project with its environments. */
  get(project: string): Promise<OrgProject> {
    return this.t.request(`/api/v1/projects/${seg(project)}`);
  }

  /** Deletes a project and its environments. */
  delete(project: string): Promise<void> {
    return this.t.request(`/api/v1/projects/${seg(project)}`, { method: "DELETE" });
  }

  /** Adds an environment (`staging`, `qa`, ...). */
  createEnvironment(project: string, name: string): Promise<ProjectEnvironment> {
    return this.t.request(`/api/v1/projects/${seg(project)}/environments`, { method: "POST", body: { name } });
  }
}

/** Organization memberships (`client.orgs.members`). */
export class OrgMembersApi {
  constructor(private readonly t: Transport) {}

  /** Members of an organization. */
  list(org: string): Promise<OrgMember[]> {
    return this.t.request(`/api/v1/orgs/${seg(org)}/members`);
  }

  /** Adds a member or replaces their roles. Roles must be assignable and within the caller's own permissions. */
  set(org: string, member: OrgMemberInput): Promise<MemberAssignment> {
    return this.t.request(`/api/v1/orgs/${seg(org)}/members`, {
      method: "PUT",
      body: { principal: member.principal, roles: member.roles ?? [], role_sets: member.roleSets ?? [] },
    });
  }

  /** Removes a member and their team memberships. */
  remove(org: string, principal: string): Promise<void> {
    return this.t.request(`/api/v1/orgs/${seg(org)}/members/remove`, { method: "POST", body: { principal } });
  }

  /** Suspends (`false`) or reactivates (`true`) a membership. */
  setActive(org: string, principal: string, active: boolean): Promise<void> {
    return this.t.request(`/api/v1/orgs/${seg(org)}/members/status`, { method: "POST", body: { principal, active } });
  }
}

/** Invitations (`client.orgs.invitations`). */
export class OrgInvitationsApi {
  constructor(private readonly t: Transport) {}

  /** Invitations of an organization, newest first. The token is never listed. */
  list(org: string): Promise<OrgInvitation[]> {
    return this.t.request(`/api/v1/orgs/${seg(org)}/invitations`);
  }

  /** Invites an email address. The returned `token` is shown once. */
  create(org: string, email: string, roles: string[] = []): Promise<InvitationCreated> {
    return this.t.request(`/api/v1/orgs/${seg(org)}/invitations`, { method: "POST", body: { email, roles } });
  }

  /** Accepts an invitation as the signed-in user (their account email must match the invited address). Single use. */
  accept(token: string): Promise<InvitationAccepted> {
    return this.t.request("/api/v1/invitations/accept", { method: "POST", body: { token } });
  }
}

/** Teams (`client.orgs.teams`). */
export class OrgTeamsApi {
  constructor(private readonly t: Transport) {}

  /** Teams of an organization. */
  list(org: string): Promise<OrgTeam[]> {
    return this.t.request(`/api/v1/orgs/${seg(org)}/teams`);
  }

  /** Creates a team, optionally under a parent team. */
  create(org: string, team: OrgTeamInput): Promise<Omit<OrgTeam, "members">> {
    return this.t.request(`/api/v1/orgs/${seg(org)}/teams`, {
      method: "POST",
      body: compact({ key: team.key, name: team.name, parent_id: team.parentId, roles: team.roles ?? [], deny: team.deny ?? [] }),
    });
  }

  /** Deletes a team and its sub-teams. `team` is the team id. */
  delete(team: string): Promise<void> {
    return this.t.request(`/api/v1/teams/${seg(team)}`, { method: "DELETE" });
  }

  /** Adds an organization member to a team. */
  addMember(team: string, principal: string): Promise<void> {
    return this.t.request(`/api/v1/teams/${seg(team)}/members`, { method: "POST", body: { principal } });
  }

  removeMember(team: string, principal: string): Promise<void> {
    return this.t.request(`/api/v1/teams/${seg(team)}/members/remove`, { method: "POST", body: { principal } });
  }
}

/** Roles of an organization, or of the realm when `org` is omitted (`client.orgs.roles`). */
export class OrgRolesApi {
  constructor(private readonly t: Transport) {}

  private path(org?: string): string {
    return org === undefined ? "/api/v1/roles" : `/api/v1/orgs/${seg(org)}/roles`;
  }

  /** Roles usable in the organization (its own and the realm's), or the realm's roles. */
  list(org?: string): Promise<PolicyRole[]> {
    return this.t.request(this.path(org));
  }

  /** Creates or replaces a role. Realm roles need a super-admin; org roles stay within the caller's own permissions. */
  put(role: OrgRoleInput, org?: string): Promise<PolicyRole> {
    return this.t.request(this.path(org), {
      method: "PUT",
      body: compact({ key: role.key, name: role.name, description: role.description, allow: role.allow ?? [], deny: role.deny ?? [] }),
    });
  }

  /** Deletes a custom role (seeded roles cannot be deleted). */
  delete(key: string, org?: string): Promise<void> {
    return this.t.request(`${this.path(org)}/${seg(key)}`, { method: "DELETE" });
  }
}

/** Role sets (named bundles of roles) of an organization, or of the realm (`client.orgs.roleSets`). */
export class OrgRoleSetsApi {
  constructor(private readonly t: Transport) {}

  private path(org?: string): string {
    return org === undefined ? "/api/v1/role-sets" : `/api/v1/orgs/${seg(org)}/role-sets`;
  }

  list(org?: string): Promise<PolicyRoleSet[]> {
    return this.t.request(this.path(org));
  }

  /** Creates or replaces a role set. */
  put(set: OrgRoleSetInput, org?: string): Promise<PolicyRoleSet> {
    return this.t.request(this.path(org), { method: "PUT", body: { key: set.key, name: set.name, roles: set.roles } });
  }
}

/** Realm-level role bindings (`client.orgs.realmBindings`, super-admins). */
export class RealmBindingsApi {
  constructor(private readonly t: Transport) {}

  list(): Promise<RealmBinding[]> {
    return this.t.request("/api/v1/realm-bindings");
  }

  /** Binds realm roles to a principal, replacing the earlier binding. */
  put(binding: OrgMemberInput): Promise<{ realm_id: string; principal: string; roles: string[]; role_sets: string[] }> {
    return this.t.request("/api/v1/realm-bindings", {
      method: "PUT",
      body: { principal: binding.principal, roles: binding.roles ?? [], role_sets: binding.roleSets ?? [] },
    });
  }
}

/** Account suspensions (`client.orgs.suspensions`, super-admins). */
export class AccountSuspensionsApi {
  constructor(private readonly t: Transport) {}

  /** Active suspensions. */
  list(): Promise<AccountSuspension[]> {
    return this.t.request("/api/v1/suspensions");
  }

  /** Suspends an account everywhere, until `until` or until lifted. */
  suspend(principal: string, opts: AccountSuspendOptions = {}): Promise<void> {
    const until = opts.until instanceof Date ? Math.floor(opts.until.getTime() / 1000) : opts.until;
    return this.t.request("/api/v1/suspensions", { method: "POST", body: compact({ principal, reason: opts.reason, until }) });
  }

  lift(principal: string): Promise<void> {
    return this.t.request("/api/v1/suspensions/lift", { method: "POST", body: { principal } });
  }
}

/** Organizations, projects, members, teams and roles: `client.orgs`. */
export class OrgsApi {
  readonly realms: TenantRealmsApi;
  readonly projects: OrgProjectsApi;
  readonly members: OrgMembersApi;
  readonly invitations: OrgInvitationsApi;
  readonly teams: OrgTeamsApi;
  readonly roles: OrgRolesApi;
  readonly roleSets: OrgRoleSetsApi;
  readonly realmBindings: RealmBindingsApi;
  readonly suspensions: AccountSuspensionsApi;

  constructor(private readonly t: Transport) {
    this.realms = new TenantRealmsApi(t);
    this.projects = new OrgProjectsApi(t);
    this.members = new OrgMembersApi(t);
    this.invitations = new OrgInvitationsApi(t);
    this.teams = new OrgTeamsApi(t);
    this.roles = new OrgRolesApi(t);
    this.roleSets = new OrgRoleSetsApi(t);
    this.realmBindings = new RealmBindingsApi(t);
    this.suspensions = new AccountSuspensionsApi(t);
  }

  /** Organizations the caller is an active member of (every organization of the realm for super-admins). */
  list(): Promise<Organization[]> {
    return this.t.request("/api/v1/orgs");
  }

  /** One organization by id. */
  get(org: string): Promise<Organization> {
    return this.t.request(`/api/v1/orgs/${seg(org)}`);
  }

  /** Creates an organization; the caller becomes its owner. */
  create(input: CreateOrgInput): Promise<Organization> {
    return this.t.request("/api/v1/orgs", { method: "POST", body: { slug: input.slug, name: input.name } });
  }

  /** Renames an organization. */
  rename(org: string, name: string): Promise<Organization> {
    return this.t.request(`/api/v1/orgs/${seg(org)}`, { method: "PATCH", body: { name } });
  }

  /** Deletes an organization with its projects, teams and memberships (tenant data rows stay). */
  delete(org: string): Promise<void> {
    return this.t.request(`/api/v1/orgs/${seg(org)}`, { method: "DELETE" });
  }
}
