import { describe, expect, it } from "vitest";
import { AccessApi } from "../src/access";
import { ForgeBase, NotFoundError, PermissionError, ValidationError } from "../src/index";
import { errBody, json, scripted } from "./helpers";

const setup = (...r: Parameters<typeof scripted>) => {
  const f = scripted(...r);
  const fb = new ForgeBase("http://x", { fetch: f.fetch, retry: { maxRetries: 0 } });
  return { f, a: new AccessApi(fb.transport) };
};
const body = (f: ReturnType<typeof scripted>, i = 0) => JSON.parse(f.calls[i]!.body!);
const reqs = (f: ReturnType<typeof scripted>) => f.calls.map((c) => `${c.method} ${c.url}`);

describe("access.me / check / can / audit", () => {
  it("me passes the org as a query parameter only when given", async () => {
    const me = { principal: "user:users/u1", realm_id: "default", org_id: "o1", super_admin: false, suspended: false, roles: ["dev"], allow: ["records:*"], deny: [], teams: ["t1"] };
    const { f, a } = setup(json(200, me), json(200, { ...me, org_id: null }));
    expect(await a.me("o1")).toEqual(me);
    await a.me();
    expect(reqs(f)).toEqual(["GET http://x/api/v1/authz/me?org=o1", "GET http://x/api/v1/authz/me"]);
  });

  it("check maps camelCase to the wire body and returns the denial reason", async () => {
    const { f, a } = setup(json(200, { allowed: true }), json(200, { allowed: false, step: "permission", reason: "no role grants records:delete" }));
    expect(await a.check({ permission: "records:read", org: "o1", resourceType: "project", resourceId: "p1", principal: "user:users/u1" })).toEqual({ allowed: true });
    expect(await a.check({ permission: "records:delete" })).toMatchObject({ allowed: false, step: "permission" });
    expect(reqs(f)).toEqual(["POST http://x/api/v1/authz/check", "POST http://x/api/v1/authz/check"]);
    expect(body(f, 0)).toEqual({ permission: "records:read", org: "o1", resource_type: "project", resource_id: "p1", principal: "user:users/u1" });
    expect(body(f, 1)).toEqual({ permission: "records:delete" });
  });

  it("can returns the boolean", async () => {
    const { f, a } = setup(json(200, { allowed: false, step: "tenant" }), json(200, { allowed: true }));
    expect(await a.can("projects:read", { org: "o1" })).toBe(false);
    expect(await a.can("projects:read")).toBe(true);
    expect(body(f, 0)).toEqual({ permission: "projects:read", org: "o1" });
  });

  it("checking as another principal needs a super-admin", async () => {
    const { a } = setup(json(403, errBody("permission_denied")));
    await expect(a.check({ permission: "x:y", principal: "user:users/u9" })).rejects.toBeInstanceOf(PermissionError);
  });

  it("audit passes the limit", async () => {
    const entry = { at: 1, realm_id: "default", principal: "user:users/u1", tenant: null, org_id: null, permission: "x:y", resource: null, step: "permission", reason: "denied", request_id: "r1", impersonator: null };
    const { f, a } = setup(json(200, [entry]), json(200, []));
    expect(await a.audit(25)).toEqual([entry]);
    await a.audit();
    expect(reqs(f)).toEqual(["GET http://x/api/v1/authz/audit?limit=25", "GET http://x/api/v1/authz/audit"]);
  });
});

describe("access.acl", () => {
  const entry = { id: "a1", resource_type: "project", resource_id: "p1", grantee: "team:t1", permission: "projects:read", effect: "allow" };

  it("lists by resource with encoded query values", async () => {
    const { f, a } = setup(json(200, [entry]));
    expect(await a.acl.list("record", "posts/1 a")).toEqual([entry]);
    expect(f.calls[0]!.url).toBe("http://x/api/v1/acl?resource_type=record&resource_id=posts%2F1+a");
  });

  it("puts and deletes", async () => {
    const { f, a } = setup(json(201, entry), json(204, null));
    expect(await a.acl.put({ resourceType: "project", resourceId: "p1", grantee: "team:t1", permission: "projects:read", effect: "allow" })).toEqual(entry);
    await a.acl.delete("a/1");
    expect(reqs(f)).toEqual(["POST http://x/api/v1/acl", "DELETE http://x/api/v1/acl/a%2F1"]);
    expect(body(f)).toEqual({ resource_type: "project", resource_id: "p1", grantee: "team:t1", permission: "projects:read", effect: "allow" });
  });

  it("an unknown resource is a NotFoundError", async () => {
    const { a } = setup(json(404, errBody("not_found", "project p9")));
    await expect(a.acl.list("project", "p9")).rejects.toBeInstanceOf(NotFoundError);
  });
});

describe("access.overrides", () => {
  it("lists with an optional org, adds (orgId omitted for realm-wide) and deletes", async () => {
    const ov = { id: "ov1", org_id: "o1", principal: "user:users/u1", permission: "billing:*", effect: "deny" };
    const { f, a } = setup(json(200, [ov]), json(200, []), json(201, ov), json(201, { ...ov, org_id: null }), json(204, null));
    expect(await a.overrides.list("o1")).toEqual([ov]);
    await a.overrides.list();
    await a.overrides.add({ orgId: "o1", principal: "user:users/u1", permission: "billing:*", effect: "deny" });
    await a.overrides.add({ principal: "user:users/u1", permission: "billing:*", effect: "deny" });
    await a.overrides.delete("ov1");
    expect(reqs(f)).toEqual([
      "GET http://x/api/v1/overrides?org=o1",
      "GET http://x/api/v1/overrides",
      "POST http://x/api/v1/overrides",
      "POST http://x/api/v1/overrides",
      "DELETE http://x/api/v1/overrides/ov1",
    ]);
    expect(body(f, 2)).toEqual({ org_id: "o1", principal: "user:users/u1", permission: "billing:*", effect: "deny" });
    expect(body(f, 3)).toEqual({ principal: "user:users/u1", permission: "billing:*", effect: "deny" });
  });
});

describe("access.policies", () => {
  const policy = { collection: "posts", list_rule: "", view_rule: "", create_rule: "@request.auth.id != ''", update_rule: null, delete_rule: null, fields: { salary: { read: "@request.auth.role = 'hr'" } } };

  it("gets, puts (omits unset rules, keeps explicit null) and deletes", async () => {
    const { f, a } = setup(json(200, policy), json(200, policy), json(204, null));
    expect(await a.policies.get("posts")).toEqual(policy);
    await a.policies.put("posts", { listRule: "", viewRule: "", createRule: "@request.auth.id != ''", updateRule: null, fields: { salary: { read: "@request.auth.role = 'hr'" } } });
    await a.policies.delete("posts");
    expect(reqs(f)).toEqual(["GET http://x/api/v1/policies/posts", "PUT http://x/api/v1/policies/posts", "DELETE http://x/api/v1/policies/posts"]);
    expect(body(f, 1)).toEqual({
      list_rule: "",
      view_rule: "",
      create_rule: "@request.auth.id != ''",
      update_rule: null,
      fields: { salary: { read: "@request.auth.role = 'hr'" } },
    });
  });

  it("an invalid rule is a ValidationError", async () => {
    const { a } = setup(json(400, errBody("invalid_rule", "unknown field")));
    await expect(a.policies.put("posts", { listRule: "nope = 1" })).rejects.toBeInstanceOf(ValidationError);
  });
});

describe("access.resourceRules", () => {
  it("lists a kind, gets, puts, deletes with `*` left unencoded", async () => {
    const catalog = { kind: "kv", fields: [{ name: "key", type: "string" }], operations: ["list", "view"], actions: ["read", "write"], items: [{ name: "*", rules: { read: "" } }] };
    const set = { kind: "kv", name: "*", rules: { read: "" } };
    const { f, a } = setup(json(200, catalog), json(200, set), json(200, set), json(204, null));
    expect(await a.resourceRules.list("kv")).toEqual(catalog);
    expect(await a.resourceRules.get("kv", "*")).toEqual(set);
    await a.resourceRules.put("kv", "sessions/eu", { read: "", write: null });
    await a.resourceRules.delete("kv", "*");
    expect(reqs(f)).toEqual([
      "GET http://x/api/v1/policies/resources/kv",
      "GET http://x/api/v1/policies/resources/kv/*",
      "PUT http://x/api/v1/policies/resources/kv/sessions%2Feu",
      "DELETE http://x/api/v1/policies/resources/kv/*",
    ]);
    expect(body(f, 2)).toEqual({ rules: { read: "", write: null } });
  });

  it("check posts the action and op and returns the decision", async () => {
    const { f, a } = setup(json(200, { decision: "allow" }), json(200, { decision: "deny", step: "rule", reason: "write is locked" }));
    expect(await a.resourceRules.check("kv", "cache", { action: "read", op: "view" })).toEqual({ decision: "allow" });
    expect(await a.resourceRules.check("kv", "cache", { action: "write", op: "update", principal: "user:users/u1" })).toMatchObject({ decision: "deny", step: "rule" });
    expect(f.calls[0]!.url).toBe("http://x/api/v1/policies/resources/kv/cache/check");
    expect(body(f, 0)).toEqual({ action: "read", op: "view" });
    expect(body(f, 1)).toEqual({ action: "write", op: "update", principal: "user:users/u1" });
  });

  it("an unknown kind is a ValidationError", async () => {
    const { a } = setup(json(400, errBody("invalid_resource_kind")));
    await expect(a.resourceRules.list("nope")).rejects.toBeInstanceOf(ValidationError);
  });
});
