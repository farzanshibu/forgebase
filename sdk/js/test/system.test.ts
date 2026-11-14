import { describe, expect, it } from "vitest";
import { ConflictError, PermissionError, Transport, ValidationError } from "../src/index";
import { SystemApi } from "../src/system";
import { errBody, json, scripted } from "./helpers";

const setup = (...r: Parameters<typeof scripted>) => {
  const f = scripted(...r);
  const t = new Transport({ baseUrl: "http://x", fetch: f.fetch });
  t.credentials = { headers: async () => ({ Authorization: "Bearer admin" }), onUnauthorized: async () => false };
  return { f, s: new SystemApi(t) };
};
const op = (status = "pending") => ({ id: "job1", kind: "database.promote", status, cancel_requested: false, cancellable: true, created_at: "t", created_by: "u" });
const lines = (calls: { method: string; url: string }[]) => calls.map((c) => `${c.method} ${c.url.replace("http://x", "")}`);

describe("system", () => {
  it("info, features and updateConfig", async () => {
    const { f, s } = setup(json(200, { name: "forgebase", version: "0.3.4" }), json(200, { config_file: "forgebase.toml", features: [], restart_pending: false }), json(200, { applied: ["a.b"], features: [], restart_required: true, written: "forgebase.toml" }));
    expect((await s.info()).version).toBe("0.3.4");
    expect((await s.features()).config_file).toBe("forgebase.toml");
    expect((await s.updateConfig({ "a.b": 1 })).restart_required).toBe(true);
    expect(lines(f.calls)).toEqual(["GET /api/system/info", "GET /api/system/features", "PATCH /api/system/config"]);
    expect(JSON.parse(f.calls[2]!.body!)).toEqual({ set: { "a.b": 1 } });
  });

  it("a bad config key is a ValidationError", async () => {
    const { s } = setup(json(400, errBody("validation_failed", "bad", { "a.b": "must be a number" })));
    const err = await s.updateConfig({ "a.b": "x" }).catch((e: unknown) => e);
    expect(err).toBeInstanceOf(ValidationError);
    expect((err as ValidationError).details).toEqual({ "a.b": "must be a number" });
  });
});

describe("system.database", () => {
  it("reads the topology, a node, placements and operations", async () => {
    const { f, s } = setup(
      json(200, { backend: "postgres", epoch: 1, nodes: [], operations: [], sets: [] }),
      json(200, { node: { id: "n1" }, lag_history: [] }),
      json(200, { items: [], next_cursor: "c2" }),
      json(200, { items: [op("running")] }),
      json(200, op("running")),
      json(200, { items: [{ id: "r1", name: "shard-a", bytes: 1 }] }),
    );
    const d = s.database;
    expect((await d.topology()).backend).toBe("postgres");
    expect((await d.node("n 1")).node.id).toBe("n1");
    expect((await d.placements({ set: "s1", project: "p", search: "ten", cursor: "c1", limit: 10 })).next_cursor).toBe("c2");
    expect(await d.operations({ active: true, limit: 5 })).toHaveLength(1);
    expect((await d.operation("job1")).status).toBe("running");
    expect((await d.retired())[0]!.id).toBe("r1");
    expect(lines(f.calls)).toEqual([
      "GET /api/system/database/topology",
      "GET /api/system/database/nodes/n%201",
      "GET /api/system/database/placements?set=s1&project=p&q=ten&cursor=c1&limit=10",
      "GET /api/system/database/operations?active=true&limit=5",
      "GET /api/system/database/operations/job1",
      "GET /api/system/database/retired",
    ]);
  });

  it("adds, updates and removes nodes", async () => {
    const { f, s } = setup(json(202, { operation: op() }), json(201, { node: { id: "n2" } }), json(200, { id: "n2", weight: 5 }), json(200, { removed: "n2" }), json(200, { primary_reachable: true }));
    const d = s.database;
    expect((await d.addNode({ kind: "local", name: "r1", set: "control", weight: 3 })).operation?.id).toBe("job1");
    expect((await d.addNode({ kind: "external", name: "r2", url: "postgres://h/db" })).node?.id).toBe("n2");
    await d.updateNode("n2", 5);
    expect(await d.removeNode("n2", "r2")).toEqual({ removed: "n2" });
    expect((await d.promoteCheck("n2")).primary_reachable).toBe(true);
    expect(lines(f.calls)).toEqual([
      "POST /api/system/database/nodes",
      "POST /api/system/database/nodes",
      "PATCH /api/system/database/nodes/n2",
      "DELETE /api/system/database/nodes/n2?confirm=r2",
      "GET /api/system/database/nodes/n2/promote-check",
    ]);
    expect(JSON.parse(f.calls[0]!.body!)).toEqual({ kind: "local", name: "r1", set: "control", weight: 3 });
    expect(JSON.parse(f.calls[2]!.body!)).toEqual({ weight: 5 });
  });

  it("promote, rebuild, move and rebalance unwrap the accepted operation", async () => {
    const { f, s } = setup(json(202, { operation: op() }), json(202, { operation: op() }), json(202, { operation: op() }), json(202, { operation: op() }), json(202, { operation: op() }));
    const d = s.database;
    expect((await d.promote("n1", { confirm: "r1", acceptDataLoss: true })).id).toBe("job1");
    await d.rebuildNode("n1");
    await d.moveTenant("t 1", { to: "set2", confirm: "t 1" });
    await d.rebalance("plan1", "plan1");
    await d.addShard({ name: "shard", node: { kind: "external", name: "n", url: "postgres://x" }, weight: 2 });
    expect(lines(f.calls)).toEqual([
      "POST /api/system/database/nodes/n1/promote",
      "POST /api/system/database/nodes/n1/rebuild",
      "POST /api/system/database/placements/t%201/move",
      "POST /api/system/database/rebalance",
      "POST /api/system/database/shards",
    ]);
    expect(JSON.parse(f.calls[0]!.body!)).toEqual({ confirm: "r1", accept_data_loss: true });
    expect(JSON.parse(f.calls[2]!.body!)).toEqual({ to: "set2", confirm: "t 1" });
    expect(JSON.parse(f.calls[3]!.body!)).toEqual({ plan_id: "plan1", confirm: "plan1" });
    expect(JSON.parse(f.calls[4]!.body!)).toEqual({ name: "shard", node: { kind: "external", name: "n", url: "postgres://x" }, weight: 2 });
  });

  it("maps a missing second factor to PermissionError and a busy shard to ConflictError", async () => {
    const { s } = setup(json(403, errBody("mfa_step_up_required")), json(409, errBody("shard_not_empty")));
    await expect(s.database.promote("n1", { confirm: "r1" })).rejects.toMatchObject({ code: "mfa_step_up_required" });
    await expect(s.database.promote("n1", { confirm: "r1" })).rejects.toBeInstanceOf(ConflictError);
  });

  it("403 is a PermissionError", async () => {
    const { s } = setup(json(403, errBody("forbidden")));
    await expect(s.database.topology()).rejects.toBeInstanceOf(PermissionError);
  });

  it("join tokens, shard updates, shard removal, rebalance plans, retired data, cancelling", async () => {
    const { f, s } = setup(
      json(201, { id: "j", token: "fbj_x", command: "forgebase node join", role: "replica", set: "control", expires_at: "t" }),
      json(200, { id: "s1" }),
      json(200, { removed: "s1" }),
      json(200, { id: "plan1", moves: [] }),
      json(200, { removed: "r1" }),
      json(200, op("running")),
    );
    const d = s.database;
    expect((await d.createJoinToken({ set: "control", role: "replica" })).token).toBe("fbj_x");
    await d.updateShard("s1", { state: "draining", weight: 4, hostsTenants: false });
    await d.removeShard("s1", "shard-a");
    expect((await d.rebalancePlan({ sets: ["a", "b"], maxMoves: 3 })).id).toBe("plan1");
    await d.deleteRetired("r1", "shard-a");
    await d.cancelOperation("job1");
    expect(lines(f.calls)).toEqual([
      "POST /api/system/database/join-tokens",
      "PATCH /api/system/database/shards/s1",
      "DELETE /api/system/database/shards/s1?confirm=shard-a",
      "POST /api/system/database/rebalance/plan",
      "DELETE /api/system/database/retired/r1?confirm=shard-a",
      "POST /api/system/database/operations/job1/cancel",
    ]);
    expect(JSON.parse(f.calls[1]!.body!)).toEqual({ state: "draining", weight: 4, hosts_tenants: false });
    expect(JSON.parse(f.calls[3]!.body!)).toEqual({ sets: ["a", "b"], max_moves: 3 });
  });

  it("waitForOperation polls until a final status", async () => {
    const { f, s } = setup(json(200, op("pending")), json(200, op("running")), json(200, op("succeeded")));
    expect((await s.database.waitForOperation("job1", { intervalMs: 1 })).status).toBe("succeeded");
    expect(f.calls).toHaveLength(3);
  });

  it("waitForOperation gives up at the timeout", async () => {
    const { s } = setup(json(200, op("running")));
    await expect(s.database.waitForOperation("job1", { intervalMs: 1, timeoutMs: 5 })).rejects.toThrow(/still running/);
  });

  it("agent calls use the agent key and no session credentials", async () => {
    const { f, s } = setup(json(200, { agent_key: "k" }), json(200, { commands: [] }), json(200, { ok: true }));
    const a = s.database.agent;
    await a.join({ token: "fbj_x", name: "n" });
    await a.commands("n1", "agentkey", { waitMs: 500, resend: true });
    await a.heartbeat("n1", "agentkey", { state: "running" });
    expect(lines(f.calls)).toEqual([
      "POST /api/system/database/agent/join",
      "GET /api/system/database/agent/nodes/n1/commands?wait_ms=500&resend=1",
      "POST /api/system/database/agent/nodes/n1/heartbeat",
    ]);
    expect(f.calls[0]!.headers.Authorization).toBeUndefined();
    expect(f.calls[1]!.headers.Authorization).toBe("Bearer agentkey");
    expect(f.calls[2]!.headers.Authorization).toBe("Bearer agentkey");
  });
});

describe("system.backups", () => {
  const manifest = { id: "snap1", created: "t", format: 1, forgebase_version: "0.3.4", includes_indexes: true, kind: "full", schema_version: 1, files: [], master_key: { included: false } };
  it("lists, creates, reads, verifies, prunes and deletes", async () => {
    const { f, s } = setup(json(200, { items: [{ id: "snap1", manifest }] }), json(201, manifest), json(200, { id: "snap1", manifest }), json(200, { ok: true, files_checked: 1, bytes_checked: 2, problems: [], warnings: [] }), json(200, { deleted: ["old"] }), json(204, null));
    const b = s.backups;
    expect(await b.list()).toHaveLength(1);
    expect((await b.create({ label: "nightly", includeIndexes: false, retention: { keepLast: 7, maxAgeDays: 30 } })).id).toBe("snap1");
    expect((await b.get("snap1")).manifest.id).toBe("snap1");
    expect((await b.verify("snap1")).ok).toBe(true);
    expect(await b.prune({ keepLast: 3 })).toEqual(["old"]);
    await b.delete("snap1");
    expect(lines(f.calls)).toEqual([
      "GET /api/v1/system/backups",
      "POST /api/v1/system/backups",
      "GET /api/v1/system/backups/snap1",
      "POST /api/v1/system/backups/snap1/verify",
      "POST /api/v1/system/backups/prune",
      "DELETE /api/v1/system/backups/snap1",
    ]);
    expect(JSON.parse(f.calls[1]!.body!)).toEqual({ label: "nightly", include_indexes: false, retention: { keep_last: 7, max_age_days: 30 } });
    expect(JSON.parse(f.calls[4]!.body!)).toEqual({ keep_last: 3 });
  });
});

describe("system.dbLogins and extensions", () => {
  const login = { name: "etl", admin: false, readonly: true, principal: "svc", role: "r", connection_limit: 5, idle_timeout_ms: 1, statement_timeout_ms: 2, has_claims: false, created_at: "t", created_by: "u" };
  it("manages logins", async () => {
    const { f, s } = setup(json(200, { items: [login] }), json(201, { login, password: "pw", hint: "h" }), json(200, login), json(200, { login, password: "pw2", hint: "h" }), json(204, null));
    const l = s.dbLogins;
    expect(await l.list()).toHaveLength(1);
    expect((await l.create({ name: "etl", readonly: true, connectionLimit: 5, statementTimeoutMs: 2, tenant: "t1", validUntil: "2030-01-01T00:00:00Z", principal: "svc", admin: false })).password).toBe("pw");
    await l.get("etl");
    expect((await l.rotate("etl", { validUntil: "2031-01-01T00:00:00Z" })).password).toBe("pw2");
    await l.delete("etl");
    expect(lines(f.calls)).toEqual(["GET /api/v1/system/db-logins", "POST /api/v1/system/db-logins", "GET /api/v1/system/db-logins/etl", "POST /api/v1/system/db-logins/etl/rotate", "DELETE /api/v1/system/db-logins/etl"]);
    expect(JSON.parse(f.calls[1]!.body!)).toEqual({ name: "etl", admin: false, readonly: true, connection_limit: 5, principal: "svc", statement_timeout_ms: 2, tenant: "t1", valid_until: "2030-01-01T00:00:00Z" });
    expect(JSON.parse(f.calls[3]!.body!)).toEqual({ valid_until: "2031-01-01T00:00:00Z" });
  });

  it("lists, installs and drops extensions", async () => {
    const ext = { name: "pgcrypto", available: true, managed_by: "user" };
    const { f, s } = setup(json(200, { items: [ext] }), json(201, ext), json(204, null));
    expect(await s.extensions.list()).toHaveLength(1);
    expect((await s.extensions.install("pgcrypto")).name).toBe("pgcrypto");
    await s.extensions.drop("pgcrypto");
    expect(lines(f.calls)).toEqual(["GET /api/v1/system/extensions", "POST /api/v1/system/extensions", "DELETE /api/v1/system/extensions/pgcrypto"]);
    expect(JSON.parse(f.calls[1]!.body!)).toEqual({ name: "pgcrypto" });
  });
});

describe("system.capabilities", () => {
  it("reads every feature with its engine and state", async () => {
    const caps = {
      backend: "sqlite",
      features: [{ feature: "graph.cypher", label: "openCypher", engine: "portable", accelerated: false, state: "portable", mode: "auto", reason: null, extension: null, limits: { max_rows: 1000 } }],
    };
    const { f, s } = setup(json(200, caps));
    const c = await s.capabilities();
    expect(c.backend).toBe("sqlite");
    expect(c.features[0]).toMatchObject({ feature: "graph.cypher", engine: "portable", state: "portable" });
    expect(lines(f.calls)).toEqual(["GET /api/system/capabilities"]);
  });

  it("needs an administrator", async () => {
    const { s } = setup(json(403, errBody("forbidden", "admin only")));
    await expect(s.capabilities()).rejects.toBeInstanceOf(PermissionError);
  });
});
