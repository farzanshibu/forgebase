/**
 * In-memory fake of `/api/system/database/*` (FB-175) for the Database topology screen (FB-176). These routes are newer
 * than the OpenAPI fixture, so they are not contract-checked until god regenerates it. Tenant moves and rebalancing
 * answer 501 `not_implemented` like the server does until FB-174 merges (`implemented: true` fakes the finished ones).
 */
import type { LagSample, NodeDetail, Placement, PromoteCheck, RebalancePlan, TopologyNode, TopologyOperation, TopologySet, TopologyView } from "../api";
import { envelope, type MockApi, type MockReply } from "./mockApi";

export interface TopologyState {
  view: TopologyView;
  history: Record<string, LagSample[]>;
  operations: TopologyOperation[];
  placements: Placement[];
  check: PromoteCheck;
  plan: RebalancePlan;
  /** Moves and rebalance work (FB-174 merged). */
  implemented: boolean;
  /** The next call of each `method path` answers 403 `mfa_step_up_required` this many times. */
  stepUp: Record<string, number>;
  /** Every call that changes something: `<method> <path>` plus its body. */
  calls: { key: string; body: unknown; query: URLSearchParams }[];
  /** Overrides an answer: return a reply to replace the default. */
  failWith?: (key: string, body: unknown) => MockReply | undefined;
  /** Placement page size. */
  pageSize: number;
  /** Runs after a change was accepted (a test can make the new server appear, as if the clone finished at once). */
  afterWrite?: (key: string, body: unknown, st: TopologyState) => void;
}

const NOW = "2026-11-11T10:00:00Z";

export const node = (id: string, set: string, over: Partial<TopologyNode> = {}): TopologyNode => ({
  id, set, name: id, kind: "local", role: "replica", state: "streaming", weight: 1, engine_version: "18.1", system_identifier: "7000", timeline: 1,
  origin: "api", in_rotation: true, lag_ms: 120, lag_bytes: 2048, replay_lsn: "0/3000060", last_error: null, agent: null, created_at: NOW, updated_at: NOW, ...over,
});

export const set = (id: string, over: Partial<TopologySet> = {}): TopologySet => ({
  id, name: id === "control" ? "Control" : id, kind: id === "control" ? "control" : "shard", number: id === "control" ? 0 : 1, state: "active", weight: 1, hosts_tenants: true, tenants: 0, est_bytes: 0, ...over,
});

export const operation = (id: string, over: Partial<TopologyOperation> = {}): TopologyOperation => ({
  id, kind: "database.node.add", status: "running", set: "control", node: "r2", progress: { percent: 40, stage: "Copying data", message: "2 of 5 GB" }, cancel_requested: false, cancellable: true,
  result: null, last_error: null, created_by: "admin", created_at: NOW, started_at: NOW, finished_at: null, ...over,
});

export const placement = (tenant: string, setId: string, over: Partial<Placement> = {}): Placement => ({
  tenant, project: "shop", organization: "acme", set: setId, state: "active", est_rows: 1000, est_bytes: 5 * 1024 * 1024, moved_at: null, ...over,
});

/** One primary and one streaming replica, no shards. */
export function simpleView(): TopologyView {
  return {
    backend: "postgres", engine_version: "18.1", epoch: 3, restart_required: null, operations: [],
    sets: [set("control", { tenants: 2, est_bytes: 10 * 1024 * 1024 })],
    nodes: [node("p1", "control", { name: "primary", role: "primary", lag_ms: null, lag_bytes: null }), node("r1", "control", { name: "r1" })],
  };
}

/** Control with a primary and a replica, plus one shard with a primary. */
export function shardedView(): TopologyView {
  return {
    ...simpleView(),
    sets: [set("control", { tenants: 2, est_bytes: 10 * 1024 * 1024 }), set("s1", { name: "eu-1", number: 1, tenants: 3, est_bytes: 30 * 1024 * 1024 })],
    nodes: [
      node("p1", "control", { name: "primary", role: "primary", lag_ms: null, lag_bytes: null }),
      node("r1", "control", { name: "r1" }),
      node("sp1", "s1", { name: "eu-1-primary", role: "primary", kind: "external", lag_ms: null, lag_bytes: null }),
    ],
  };
}

export function installTopology(mock: MockApi, init: Partial<TopologyState> = {}): TopologyState {
  const st: TopologyState = {
    view: simpleView(), history: {}, operations: [], placements: [], implemented: false, stepUp: {}, calls: [], pageSize: 2,
    check: { replayed: "0/3000060", primary_lsn: "0/3000060", behind_bytes: 0, primary_reachable: true },
    plan: { plan_id: "plan-7", moves: [{ tenant: "env-a", project: "shop", from: "control", to: "s1", est_bytes: 5 * 1024 * 1024, freeze_estimate_ms: 1500 }], est_bytes: 5 * 1024 * 1024, freeze_estimate_ms: 1500 },
    ...init,
  };
  const B = "/api/system/database";
  const step = (key: string): MockReply | undefined => {
    if ((st.stepUp[key] ?? 0) > 0) {
      st.stepUp[key]! -= 1;
      return envelope(403, "mfa_step_up_required", "sign in again with the second factor");
    }
    return undefined;
  };
  const write = (method: string, template: string, h: (r: { params: Record<string, string>; body: unknown; query: URLSearchParams }) => MockReply) =>
    mock.on(method, `${B}${template}`, (r) => {
      const key = `${method} ${template}`;
      const bad = step(key) ?? st.failWith?.(key, r.body);
      if (bad) return bad;
      st.calls.push({ key, body: r.body, query: r.query });
      const reply = h(r);
      st.afterWrite?.(key, r.body, st);
      return reply;
    });
  const accepted = (op: Partial<TopologyOperation>) => {
    const o = operation(`op${st.operations.length + 1}`, op);
    st.operations.unshift(o);
    st.view.operations = st.operations.filter((x) => ["running", "queued"].includes(x.status));
    return o;
  };

  mock.on("GET", `${B}/topology`, () => st.failWith?.("GET /topology", undefined) ?? { status: 200, body: { ...st.view, operations: st.operations.filter((o) => ["running", "queued"].includes(o.status)) } });
  mock.on("GET", `${B}/nodes/{id}`, ({ params }) => {
    const n = st.view.nodes.find((x) => x.id === params.id);
    if (!n) return envelope(404, "not_found", "no such node");
    const detail: NodeDetail = { node: n, lag_history: st.history[n.id] ?? [] };
    return { status: 200, body: detail };
  });
  mock.on("GET", `${B}/nodes/{id}/promote-check`, () => ({ status: 200, body: st.check }));
  write("POST", "/nodes", ({ body }) => {
    const b = body as { set?: string; name: string; kind: string };
    if (b.kind === "external") {
      const n = node(`n-${b.name}`, b.set ?? "control", { name: b.name, kind: "external" });
      st.view.nodes.push(n);
      return { status: 201, body: { node: n } };
    }
    return { status: 202, body: { operation: accepted({ kind: "database.node.add", node: b.name, set: b.set ?? "control" }) } };
  });
  write("PATCH", "/nodes/{id}", ({ params, body }) => {
    const n = st.view.nodes.find((x) => x.id === params.id)!;
    n.weight = (body as { weight: number }).weight;
    return { status: 200, body: n };
  });
  write("DELETE", "/nodes/{id}", ({ params, query }) => {
    if (query.get("confirm") !== st.view.nodes.find((x) => x.id === params.id)?.name) return envelope(400, "confirm_mismatch", "type the node name");
    st.view.nodes = st.view.nodes.filter((x) => x.id !== params.id);
    return { status: 200, body: { removed: params.id } };
  });
  write("POST", "/nodes/{id}/promote", ({ params }) => ({ status: 202, body: { operation: accepted({ kind: "database.promote", node: params.id }) } }));
  write("POST", "/nodes/{id}/rebuild", ({ params }) => ({ status: 202, body: { operation: accepted({ kind: "database.node.rebuild", node: params.id }) } }));
  write("POST", "/join-tokens", ({ body }) => ({
    status: 201,
    body: { id: "jt1", token: "fbj_secret_token", set: (body as { set: string }).set, role: "replica", expires_at: "2026-11-11T10:15:00Z", command: "forgebase node join --token fbj_secret_token --server https://example.test" },
  }));
  write("POST", "/shards", ({ body }) => ({ status: 202, body: { operation: accepted({ kind: "database.shard.add", set: (body as { name: string }).name, node: (body as { node: { name: string } }).node.name }) } }));
  write("PATCH", "/shards/{id}", ({ params, body }) => {
    const s = st.view.sets.find((x) => x.id === params.id)!;
    Object.assign(s, body);
    return { status: 200, body: s };
  });
  write("DELETE", "/shards/{id}", ({ params }) => {
    const s = st.view.sets.find((x) => x.id === params.id)!;
    if (s.tenants > 0) return envelope(409, "shard_not_empty", "shard holds tenants");
    st.view.sets = st.view.sets.filter((x) => x.id !== params.id);
    return { status: 200, body: { removed: params.id } };
  });
  mock.on("GET", `${B}/placements`, ({ query }) => {
    let items = st.placements;
    const setF = query.get("set"), proj = query.get("project"), q = query.get("q");
    if (setF) items = items.filter((p) => p.set === setF);
    if (proj) items = items.filter((p) => p.project === proj);
    if (q) items = items.filter((p) => [p.tenant, p.project, p.organization].join(" ").includes(q));
    const from = Number(query.get("cursor") ?? 0);
    const page = items.slice(from, from + st.pageSize);
    return { status: 200, body: { items: page, next_cursor: from + st.pageSize < items.length ? String(from + st.pageSize) : null } };
  });
  write("POST", "/placements/{tenant}/move", ({ params, body }) => {
    if (!st.implemented) return envelope(501, "not_implemented", "tenant moves arrive with FB-174");
    return { status: 202, body: { operation: accepted({ kind: "database.tenant.move", node: null, set: (body as { to: string }).to, progress: { percent: 0, stage: `Moving ${params.tenant}` } }) } };
  });
  write("POST", "/rebalance/plan", () => (st.implemented ? { status: 200, body: st.plan } : envelope(501, "not_implemented", "rebalance arrives with FB-174")));
  write("POST", "/rebalance", () => (st.implemented ? { status: 202, body: { operation: accepted({ kind: "database.rebalance" }) } } : envelope(501, "not_implemented", "rebalance arrives with FB-174")));
  mock.on("GET", `${B}/operations`, ({ query }) => ({ status: 200, body: { items: query.get("active") ? st.operations.filter((o) => ["running", "queued"].includes(o.status)) : st.operations } }));
  mock.on("GET", `${B}/operations/{id}`, ({ params }) => {
    const o = st.operations.find((x) => x.id === params.id);
    return o ? { status: 200, body: o } : envelope(404, "operation_not_found", "no such operation");
  });
  write("POST", "/operations/{id}/cancel", ({ params }) => {
    const o = st.operations.find((x) => x.id === params.id)!;
    o.cancel_requested = true;
    o.status = "cancelled";
    return { status: 200, body: o };
  });
  return st;
}

/** The ids a mutating `method template` was called with, in order. */
export const bodiesOf = (st: TopologyState, key: string) => st.calls.filter((c) => c.key === key).map((c) => c.body);
