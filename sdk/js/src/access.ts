import type { Transport } from "./http";

/*
 * Authorization (`/api/v1`, FB-21): permission checks, resource ACLs, per-principal overrides, collection rules and
 * engine resource rules: `client.access`. The OpenAPI document types these responses as plain objects, so the wire
 * types are written by hand; they follow the server's JSON exactly (snake_case).
 */

export type AccessEffect = "allow" | "deny";

/** The caller's effective access (`access.me`). */
export interface AccessSummary {
  /** `user:<collection>/<id>`, `service:<id>` or `anonymous`. */
  principal: string;
  realm_id: string;
  org_id: string | null;
  super_admin: boolean;
  suspended: boolean;
  roles: string[];
  /** Permission patterns granted. */
  allow: string[];
  /** Permission patterns denied (a deny wins). */
  deny: string[];
  /** Team ids the caller belongs to in the organization. */
  teams: string[];
}

export interface AccessCheckInput {
  /** Permission key, e.g. `records:create`, `projects:read`. */
  permission: string;
  /** Organization (default: the caller's `X-Forgebase-Org`). */
  org?: string;
  resourceType?: string;
  resourceId?: string;
  /** Check as this principal (`user:<collection>/<id>`; super-admins only). */
  principal?: string;
}

export interface AccessDecision {
  allowed: boolean;
  /** The pipeline step that denied (`authentication`, `tenant`, `permission`, ...). */
  step?: string;
  reason?: string;
}

/** One denial in the authorization audit log (`access.audit`, super-admins). */
export interface AccessAuditEntry {
  /** Unix seconds. */
  at: number;
  realm_id: string | null;
  principal: string | null;
  tenant: string | null;
  org_id: string | null;
  permission: string | null;
  resource: string | null;
  step: string | null;
  reason: string | null;
  request_id: string | null;
  impersonator: string | null;
}

export interface AccessAclEntry {
  id: string;
  realm_id?: string;
  /** `project`, `environment`, `collection`, `record`, `file`, ... */
  resource_type: string;
  /** `<collection>/<id>` for records. */
  resource_id: string;
  /** A principal key, `team:<id>` or `role:<key>`. */
  grantee: string;
  /** Permission pattern. */
  permission: string;
  effect: AccessEffect;
}

export interface AccessAclInput {
  resourceType: string;
  resourceId: string;
  grantee: string;
  permission: string;
  effect: AccessEffect;
}

export interface PermissionOverride {
  id: string;
  realm_id?: string;
  /** `null`: realm-wide. */
  org_id: string | null;
  principal: string;
  permission: string;
  effect: AccessEffect;
}

export interface PermissionOverrideInput {
  /** Omit for a realm-wide override (super-admins). */
  orgId?: string;
  principal: string;
  permission: string;
  effect: AccessEffect;
}

/** Read and write rule of one field. */
export interface CollectionFieldRules {
  read?: string | null;
  write?: string | null;
}

/**
 * Rules of a collection. A rule is an expression; `null` locks the operation and `""` opens it to everyone
 * (`list_rule: ""` is a public list).
 */
export interface CollectionPolicy {
  collection: string;
  list_rule: string | null;
  view_rule: string | null;
  create_rule: string | null;
  update_rule: string | null;
  delete_rule: string | null;
  fields: Record<string, CollectionFieldRules>;
}

export interface CollectionPolicyInput {
  listRule?: string | null;
  viewRule?: string | null;
  createRule?: string | null;
  updateRule?: string | null;
  deleteRule?: string | null;
  /** Rules by field name. */
  fields?: Record<string, CollectionFieldRules>;
}

/** Rules of one engine resource, by action (`list`, `view`, `create`, `update`, `delete`, `read`, `write`). */
export interface ResourceRuleSet {
  kind: string;
  /** Resource name; `*` holds the rules of every resource of the kind without rules of its own. */
  name: string;
  rules: Record<string, string | null>;
}

/** What rules can be written for a kind, and which resources have some (`resourceRules.list`). */
export interface ResourceRuleCatalog {
  kind: string;
  /** Item fields a rule may read. */
  fields: { name: string; type: string }[];
  operations: string[];
  /** Actions `resourceRules.check` accepts. */
  actions: string[];
  items: { name: string; rules: Record<string, string | null> }[];
}

export interface ResourceCheckInput {
  action: string;
  /** `list`, `view`, `create`, `update` or `delete`. */
  op: "list" | "view" | "create" | "update" | "delete";
  /** Check as this principal (super-admins only). */
  principal?: string;
}

/** `filter`: allowed for the rows the rule matches. */
export interface ResourceDecision {
  decision: "allow" | "filter" | "deny";
  step?: string;
  reason?: string;
}

const seg = encodeURIComponent;

/** Drops `undefined` so unset fields stay off the wire (an explicit `null` is kept: it locks a rule). */
const compact = (o: Record<string, unknown>): Record<string, unknown> => Object.fromEntries(Object.entries(o).filter(([, v]) => v !== undefined));

/** Resource ACL entries (`client.access.acl`). */
export class AccessAclApi {
  constructor(private readonly t: Transport) {}

  /** Entries of one resource. */
  list(resourceType: string, resourceId: string): Promise<AccessAclEntry[]> {
    return this.t.request("/api/v1/acl", { query: { resource_type: resourceType, resource_id: resourceId } });
  }

  /** Grants or denies a permission on one resource to a principal, team (`team:<id>`) or role (`role:<key>`). */
  put(entry: AccessAclInput): Promise<AccessAclEntry> {
    return this.t.request("/api/v1/acl", {
      method: "POST",
      body: { resource_type: entry.resourceType, resource_id: entry.resourceId, grantee: entry.grantee, permission: entry.permission, effect: entry.effect },
    });
  }

  delete(id: string): Promise<void> {
    return this.t.request(`/api/v1/acl/${seg(id)}`, { method: "DELETE" });
  }
}

/** Per-principal allow/deny overrides (`client.access.overrides`). */
export class AccessOverridesApi {
  constructor(private readonly t: Transport) {}

  /** Overrides of an organization, or the realm's when `org` is omitted (super-admins). */
  list(org?: string): Promise<PermissionOverride[]> {
    return this.t.request("/api/v1/overrides", { query: { org } });
  }

  add(o: PermissionOverrideInput): Promise<PermissionOverride> {
    return this.t.request("/api/v1/overrides", {
      method: "POST",
      body: compact({ org_id: o.orgId, principal: o.principal, permission: o.permission, effect: o.effect }),
    });
  }

  delete(id: string): Promise<void> {
    return this.t.request(`/api/v1/overrides/${seg(id)}`, { method: "DELETE" });
  }
}

/** Rules of collections (`client.access.policies`, super-admins). */
export class AccessPoliciesApi {
  constructor(private readonly t: Transport) {}

  get(collection: string): Promise<CollectionPolicy> {
    return this.t.request(`/api/v1/policies/${seg(collection)}`);
  }

  /** Replaces the rules of a collection. An omitted rule is locked (`null`). */
  put(collection: string, policy: CollectionPolicyInput): Promise<CollectionPolicy> {
    return this.t.request(`/api/v1/policies/${seg(collection)}`, {
      method: "PUT",
      body: compact({
        list_rule: policy.listRule,
        view_rule: policy.viewRule,
        create_rule: policy.createRule,
        update_rule: policy.updateRule,
        delete_rule: policy.deleteRule,
        fields: policy.fields,
      }),
    });
  }

  /** Removes the rules: the collection is locked again. */
  delete(collection: string): Promise<void> {
    return this.t.request(`/api/v1/policies/${seg(collection)}`, { method: "DELETE" });
  }
}

/** Rules of engine resources: KV namespaces, buckets, queues, ... (`client.access.resourceRules`, super-admins). */
export class AccessResourceRulesApi {
  constructor(private readonly t: Transport) {}

  /** What the kind supports, and every resource of it with rules. */
  list(kind: string): Promise<ResourceRuleCatalog> {
    return this.t.request(`/api/v1/policies/resources/${seg(kind)}`);
  }

  /** Rules of one resource; `name` `*` is the default of the kind. */
  get(kind: string, name: string): Promise<ResourceRuleSet> {
    return this.t.request(`/api/v1/policies/resources/${seg(kind)}/${seg(name)}`);
  }

  /** Replaces the rules of one resource: action to rule (`null` locks, `""` opens to everyone). */
  put(kind: string, name: string, rules: Record<string, string | null>): Promise<ResourceRuleSet> {
    return this.t.request(`/api/v1/policies/resources/${seg(kind)}/${seg(name)}`, { method: "PUT", body: { rules } });
  }

  /** Removes every rule of the resource. */
  delete(kind: string, name: string): Promise<void> {
    return this.t.request(`/api/v1/policies/resources/${seg(kind)}/${seg(name)}`, { method: "DELETE" });
  }

  /** Dry run: would this action be allowed on the resource? */
  check(kind: string, name: string, input: ResourceCheckInput): Promise<ResourceDecision> {
    return this.t.request(`/api/v1/policies/resources/${seg(kind)}/${seg(name)}/check`, {
      method: "POST",
      body: compact({ action: input.action, op: input.op, principal: input.principal }),
    });
  }
}

/** Permission checks, ACLs, overrides and rules: `client.access`. */
export class AccessApi {
  readonly acl: AccessAclApi;
  readonly overrides: AccessOverridesApi;
  readonly policies: AccessPoliciesApi;
  readonly resourceRules: AccessResourceRulesApi;

  constructor(private readonly t: Transport) {
    this.acl = new AccessAclApi(t);
    this.overrides = new AccessOverridesApi(t);
    this.policies = new AccessPoliciesApi(t);
    this.resourceRules = new AccessResourceRulesApi(t);
  }

  /** The caller's roles and effective permissions in an organization (default: the caller's current one). */
  me(org?: string): Promise<AccessSummary> {
    return this.t.request("/api/v1/authz/me", { query: { org } });
  }

  /** Dry-runs a permission check. On a denial, `step` and `reason` say why. */
  check(input: AccessCheckInput): Promise<AccessDecision> {
    return this.t.request("/api/v1/authz/check", {
      method: "POST",
      body: compact({ permission: input.permission, org: input.org, resource_type: input.resourceType, resource_id: input.resourceId, principal: input.principal }),
    });
  }

  /** `check`, reduced to the boolean. */
  async can(permission: string, opts: Omit<AccessCheckInput, "permission"> = {}): Promise<boolean> {
    return (await this.check({ ...opts, permission })).allowed;
  }

  /** Recent authorization denials, newest first (super-admins). `limit` is clamped to 1..1000 by the server (default 100). */
  audit(limit?: number): Promise<AccessAuditEntry[]> {
    return this.t.request("/api/v1/authz/audit", { query: { limit } });
  }
}
