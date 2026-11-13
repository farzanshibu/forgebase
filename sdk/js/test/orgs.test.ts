import { describe, expect, it } from "vitest";
import { AuthenticationError, ConflictError, ForgeBase, NotFoundError, PermissionError, ValidationError } from "../src/index";
import { OrgsApi } from "../src/orgs";
import { errBody, json, scripted } from "./helpers";

const setup = (...r: Parameters<typeof scripted>) => {
  const f = scripted(...r);
  const fb = new ForgeBase("http://x", { fetch: f.fetch, retry: { maxRetries: 0 } });
  return { f, o: new OrgsApi(fb.transport) };
};
const body = (f: ReturnType<typeof scripted>, i = 0) => JSON.parse(f.calls[i]!.body!);

const org = { id: "o1", realm_id: "default", slug: "acme", name: "Acme" };
const project = { id: "p1", realm_id: "default", org_id: "o1", owner_user: null, slug: "web", name: "Web" };

describe("orgs", () => {
  it("lists, gets, creates, renames and deletes organizations", async () => {
    const { f, o } = setup(json(200, [org]), json(200, org), json(201, org), json(200, { ...org, name: "Acme Inc" }), json(204, null));
    expect(await o.list()).toEqual([org]);
    expect(await o.get("o/1")).toEqual(org);
    expect(await o.create({ slug: "acme", name: "Acme" })).toEqual(org);
    expect((await o.rename("o1", "Acme Inc")).name).toBe("Acme Inc");
    expect(await o.delete("o1")).toBeUndefined();
    expect(f.calls.map((c) => `${c.method} ${c.url}`)).toEqual([
      "GET http://x/api/v1/orgs",
      "GET http://x/api/v1/orgs/o%2F1",
      "POST http://x/api/v1/orgs",
      "PATCH http://x/api/v1/orgs/o1",
      "DELETE http://x/api/v1/orgs/o1",
    ]);
    expect(body(f, 2)).toEqual({ slug: "acme", name: "Acme" });
    expect(body(f, 3)).toEqual({ name: "Acme Inc" });
  });

  it("maps errors", async () => {
    const { o } = setup(json(409, errBody("slug_taken")), json(403, errBody("permission_denied")), json(404, errBody("not_found")), json(401, errBody("unauthenticated")));
    await expect(o.create({ slug: "acme", name: "A" })).rejects.toBeInstanceOf(ConflictError);
    await expect(o.get("o1")).rejects.toBeInstanceOf(PermissionError);
    await expect(o.delete("nope")).rejects.toBeInstanceOf(NotFoundError);
    await expect(o.list()).rejects.toBeInstanceOf(AuthenticationError);
  });

  it("surfaces validation errors with their code", async () => {
    const { o } = setup(json(400, errBody("invalid_slug", "slug is invalid")));
    await expect(o.create({ slug: "A B", name: "x" })).rejects.toMatchObject({ code: "invalid_slug", status: 400 });
    const { o: o2 } = setup(json(400, errBody("invalid_slug")));
    await expect(o2.create({ slug: "!", name: "x" })).rejects.toBeInstanceOf(ValidationError);
  });
});

describe("orgs.realms", () => {
  it("lists and creates realms", async () => {
    const { f, o } = setup(json(200, [{ id: "default", name: "Default" }]), json(201, { id: "eu", name: "EU" }));
    expect(await o.realms.list()).toEqual([{ id: "default", name: "Default" }]);
    expect(await o.realms.create("eu", "EU")).toEqual({ id: "eu", name: "EU" });
    expect(f.calls[0]).toMatchObject({ method: "GET", url: "http://x/api/v1/realms" });
    expect(f.calls[1]).toMatchObject({ method: "POST", url: "http://x/api/v1/realms" });
    expect(body(f, 1)).toEqual({ id: "eu", name: "EU" });
  });
});

describe("orgs.projects", () => {
  it("uses the org route with an org and the personal route without", async () => {
    const { f, o } = setup(json(200, [project]), json(200, []), json(201, project), json(201, { ...project, org_id: null }));
    expect(await o.projects.list("o1")).toEqual([project]);
    expect(await o.projects.list()).toEqual([]);
    await o.projects.create({ slug: "web", name: "Web" }, "o1");
    await o.projects.create({ slug: "me", name: "Mine" });
    expect(f.calls.map((c) => `${c.method} ${c.url}`)).toEqual([
      "GET http://x/api/v1/orgs/o1/projects",
      "GET http://x/api/v1/projects",
      "POST http://x/api/v1/orgs/o1/projects",
      "POST http://x/api/v1/projects",
    ]);
    expect(body(f, 3)).toEqual({ slug: "me", name: "Mine" });
  });

  it("gets with environments, deletes and adds an environment", async () => {
    const env = { id: "e1", project_id: "p1", name: "staging" };
    const { f, o } = setup(json(200, { ...project, environments: [env] }), json(204, null), json(201, env));
    expect((await o.projects.get("p1")).environments).toEqual([env]);
    await o.projects.delete("p1");
    expect(await o.projects.createEnvironment("p1", "staging")).toEqual(env);
    expect(f.calls.map((c) => `${c.method} ${c.url}`)).toEqual([
      "GET http://x/api/v1/projects/p1",
      "DELETE http://x/api/v1/projects/p1",
      "POST http://x/api/v1/projects/p1/environments",
    ]);
    expect(body(f, 2)).toEqual({ name: "staging" });
  });
});

describe("orgs.members", () => {
  it("lists, sets with wire field names, removes and toggles status", async () => {
    const member = { id: "m1", scope_id: "o1", principal: "user:users/u1", roles: ["admin"], role_sets: [], status: "active" };
    const { f, o } = setup(json(200, [member]), json(200, { org_id: "o1", principal: "user:users/u1", roles: ["admin"], role_sets: ["eng"], status: "active" }), json(204, null), json(204, null), json(204, null));
    expect(await o.members.list("o1")).toEqual([member]);
    expect((await o.members.set("o1", { principal: "user:users/u1", roles: ["admin"], roleSets: ["eng"] })).role_sets).toEqual(["eng"]);
    await o.members.remove("o1", "user:users/u1");
    await o.members.setActive("o1", "user:users/u1", false);
    await o.members.setActive("o1", "user:users/u1", true);
    expect(f.calls.map((c) => `${c.method} ${c.url}`)).toEqual([
      "GET http://x/api/v1/orgs/o1/members",
      "PUT http://x/api/v1/orgs/o1/members",
      "POST http://x/api/v1/orgs/o1/members/remove",
      "POST http://x/api/v1/orgs/o1/members/status",
      "POST http://x/api/v1/orgs/o1/members/status",
    ]);
    expect(body(f, 1)).toEqual({ principal: "user:users/u1", roles: ["admin"], role_sets: ["eng"] });
    expect(body(f, 2)).toEqual({ principal: "user:users/u1" });
    expect(body(f, 3)).toEqual({ principal: "user:users/u1", active: false });
    expect(body(f, 4).active).toBe(true);
  });

  it("defaults roles and role sets to empty lists", async () => {
    const { f, o } = setup(json(200, {}));
    await o.members.set("o1", { principal: "service:s1" });
    expect(body(f)).toEqual({ principal: "service:s1", roles: [], role_sets: [] });
  });

  it("a role above the caller's own is a permission error", async () => {
    const { o } = setup(json(403, errBody("permission_denied", "role exceeds your permissions")));
    await expect(o.members.set("o1", { principal: "user:users/u1", roles: ["owner"] })).rejects.toBeInstanceOf(PermissionError);
  });
});

describe("orgs.invitations", () => {
  it("lists, creates (token shown once) and accepts", async () => {
    const inv = { id: "i1", org_id: "o1", email: "a@x.io", roles: ["viewer"], expires_at: 1700000000, status: "pending" };
    const { f, o } = setup(json(200, [inv]), json(201, { ...inv, token: "tok" }), json(200, { org_id: "o1", principal: "user:users/u2", roles: ["viewer"] }));
    expect(await o.invitations.list("o1")).toEqual([inv]);
    expect((await o.invitations.create("o1", "a@x.io", ["viewer"])).token).toBe("tok");
    expect(await o.invitations.accept("tok")).toMatchObject({ org_id: "o1" });
    expect(f.calls.map((c) => `${c.method} ${c.url}`)).toEqual([
      "GET http://x/api/v1/orgs/o1/invitations",
      "POST http://x/api/v1/orgs/o1/invitations",
      "POST http://x/api/v1/invitations/accept",
    ]);
    expect(body(f, 1)).toEqual({ email: "a@x.io", roles: ["viewer"] });
    expect(body(f, 2)).toEqual({ token: "tok" });
  });

  it("create defaults to no roles; an unknown token is a NotFoundError", async () => {
    const { f, o } = setup(json(201, {}), json(404, errBody("not_found", "invitation")));
    await o.invitations.create("o1", "b@x.io");
    expect(body(f)).toEqual({ email: "b@x.io", roles: [] });
    await expect(o.invitations.accept("bad")).rejects.toBeInstanceOf(NotFoundError);
  });
});

describe("orgs.teams", () => {
  it("lists, creates (parent only when given), deletes and manages members", async () => {
    const team = { id: "t1", org_id: "o1", parent_id: null, key: "eng", name: "Engineering", roles: ["dev"], deny: [], members: [] };
    const { f, o } = setup(json(200, [team]), json(201, team), json(201, team), json(204, null), json(204, null), json(204, null));
    expect(await o.teams.list("o1")).toEqual([team]);
    await o.teams.create("o1", { key: "eng", name: "Engineering", roles: ["dev"] });
    await o.teams.create("o1", { key: "fe", name: "Frontend", parentId: "t1", deny: ["billing:*"] });
    await o.teams.delete("t1");
    await o.teams.addMember("t1", "user:users/u1");
    await o.teams.removeMember("t1", "user:users/u1");
    expect(f.calls.map((c) => `${c.method} ${c.url}`)).toEqual([
      "GET http://x/api/v1/orgs/o1/teams",
      "POST http://x/api/v1/orgs/o1/teams",
      "POST http://x/api/v1/orgs/o1/teams",
      "DELETE http://x/api/v1/teams/t1",
      "POST http://x/api/v1/teams/t1/members",
      "POST http://x/api/v1/teams/t1/members/remove",
    ]);
    expect(body(f, 1)).toEqual({ key: "eng", name: "Engineering", roles: ["dev"], deny: [] });
    expect(body(f, 2)).toEqual({ key: "fe", name: "Frontend", parent_id: "t1", roles: [], deny: ["billing:*"] });
    expect(body(f, 4)).toEqual({ principal: "user:users/u1" });
  });

  it("adding a non-member is a ValidationError", async () => {
    const { o } = setup(json(400, errBody("not_a_member")));
    await expect(o.teams.addMember("t1", "user:users/zz")).rejects.toBeInstanceOf(ValidationError);
  });
});

describe("orgs.roles and roleSets", () => {
  const role = { id: "r1", realm_id: "default", org_id: "o1", key: "dev", name: "Developer", description: "", allow: ["records:*"], deny: [], system: false, assignable: true };

  it("org scope uses /orgs/{org}, realm scope the top-level routes", async () => {
    const { f, o } = setup(json(200, [role]), json(200, []), json(200, role), json(200, role), json(204, null), json(204, null));
    expect(await o.roles.list("o1")).toEqual([role]);
    await o.roles.list();
    await o.roles.put({ key: "dev", name: "Developer", allow: ["records:*"] }, "o1");
    await o.roles.put({ key: "auditor", description: "reads audit" });
    await o.roles.delete("dev", "o1");
    await o.roles.delete("a/b");
    expect(f.calls.map((c) => `${c.method} ${c.url}`)).toEqual([
      "GET http://x/api/v1/orgs/o1/roles",
      "GET http://x/api/v1/roles",
      "PUT http://x/api/v1/orgs/o1/roles",
      "PUT http://x/api/v1/roles",
      "DELETE http://x/api/v1/orgs/o1/roles/dev",
      "DELETE http://x/api/v1/roles/a%2Fb",
    ]);
    expect(body(f, 2)).toEqual({ key: "dev", name: "Developer", allow: ["records:*"], deny: [] });
    expect(body(f, 3)).toEqual({ key: "auditor", description: "reads audit", allow: [], deny: [] });
  });

  it("deleting a seeded role is a ValidationError", async () => {
    const { o } = setup(json(400, errBody("system_role")));
    await expect(o.roles.delete("owner", "o1")).rejects.toMatchObject({ code: "system_role" });
  });

  it("role sets list and put in both scopes", async () => {
    const set = { id: "s1", realm_id: "default", org_id: null, key: "eng", name: "Eng", roles: ["dev"] };
    const { f, o } = setup(json(200, [set]), json(200, [set]), json(200, set), json(200, set));
    await o.roleSets.list("o1");
    await o.roleSets.list();
    await o.roleSets.put({ key: "eng", name: "Eng", roles: ["dev"] }, "o1");
    await o.roleSets.put({ key: "eng", name: "Eng", roles: ["dev"] });
    expect(f.calls.map((c) => `${c.method} ${c.url}`)).toEqual([
      "GET http://x/api/v1/orgs/o1/role-sets",
      "GET http://x/api/v1/role-sets",
      "PUT http://x/api/v1/orgs/o1/role-sets",
      "PUT http://x/api/v1/role-sets",
    ]);
    expect(body(f, 2)).toEqual({ key: "eng", name: "Eng", roles: ["dev"] });
  });
});

describe("orgs.realmBindings", () => {
  it("lists and puts", async () => {
    const { f, o } = setup(json(200, [{ principal: "user:users/u1", roles: ["admin"], role_sets: [] }]), json(200, { realm_id: "default", principal: "user:users/u1", roles: ["admin"], role_sets: [] }));
    expect(await o.realmBindings.list()).toHaveLength(1);
    expect((await o.realmBindings.put({ principal: "user:users/u1", roles: ["admin"] })).realm_id).toBe("default");
    expect(f.calls[0]).toMatchObject({ method: "GET", url: "http://x/api/v1/realm-bindings" });
    expect(f.calls[1]).toMatchObject({ method: "PUT", url: "http://x/api/v1/realm-bindings" });
    expect(body(f, 1)).toEqual({ principal: "user:users/u1", roles: ["admin"], role_sets: [] });
  });
});

describe("orgs.suspensions", () => {
  it("lists, suspends (Date to Unix seconds, unset fields omitted) and lifts", async () => {
    const { f, o } = setup(json(200, [{ principal: "user:users/u1", reason: "abuse", suspended_by: "user:admins/a", created_at: 1, until: null }]), json(204, null), json(204, null), json(204, null));
    expect((await o.suspensions.list())[0]!.reason).toBe("abuse");
    await o.suspensions.suspend("user:users/u1", { reason: "abuse", until: new Date(1_700_000_000_500) });
    await o.suspensions.suspend("user:users/u2");
    await o.suspensions.lift("user:users/u1");
    expect(f.calls.map((c) => `${c.method} ${c.url}`)).toEqual([
      "GET http://x/api/v1/suspensions",
      "POST http://x/api/v1/suspensions",
      "POST http://x/api/v1/suspensions",
      "POST http://x/api/v1/suspensions/lift",
    ]);
    expect(body(f, 1)).toEqual({ principal: "user:users/u1", reason: "abuse", until: 1_700_000_000 });
    expect(body(f, 2)).toEqual({ principal: "user:users/u2" });
    expect(body(f, 3)).toEqual({ principal: "user:users/u1" });
  });

  it("listing needs a super-admin", async () => {
    const { o } = setup(json(403, errBody("permission_denied")));
    await expect(o.suspensions.list()).rejects.toBeInstanceOf(PermissionError);
  });
});
