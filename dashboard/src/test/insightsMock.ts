/**
 * In-memory fake of `/api/v1/insights/*` (ADR-0036), built from crates/forgebase-insights (http.rs, queries.rs,
 * usage.rs, functions.rs). The SDK OpenAPI copies are stale, so these routes are custom (`mock.on`).
 */
import type { FunctionStat, Invocation, InsightAuditEntry, InsightsStatus, Regression, SlowStatement, StatementStat, UsageRow } from "../api";
import { envelope, type MockApi, type MockRequest } from "./mockApi";

const T0 = "2026-11-14T10:00:00Z";

export const stmt = (o: Partial<StatementStat> = {}): StatementStat => ({
  fingerprint: "fp-users", statement: "select name from users where id = ? and name = ?", area: "auth", backend: "sqlite", calls: 1200, errors: 3, error_rate: 0.0025, writes: 0, slow: 4, rows: 1200,
  total_ms: 5400.5, mean_ms: 4.5, min_ms: 0.2, max_ms: 120, p50_ms: 2.1, p95_ms: 12.3, p99_ms: 88.8, ...o,
});

export const fnStat = (o: Partial<FunctionStat> = {}): FunctionStat => ({
  function: "checkout", version: 3, invocations: 500, errors: 12, error_rate: 0.024, timeouts: 2, cold_starts: 7, mean_ms: 15, p50_ms: 10, p95_ms: 40, p99_ms: 90, max_ms: 150,
  fuel_mean: 20_000, fuel_max: 800_000, fuel_limit: 1_000_000, fuel_utilisation: 0.8, memory_max_bytes: 4_194_304, memory_limit_bytes: 67_108_864, host_calls_per_invocation: 3.5,
  host: [{ op: "db.query", count: 1200, errors: 3, total_ms: 600, mean_ms: 0.5, max_ms: 20, share: 0.7 }, { op: "kv.get", count: 500, errors: 0, total_ms: 50, mean_ms: 0.1, max_ms: 1, share: 0.3 }], ...o,
});

export const invocation = (o: Partial<Invocation> = {}): Invocation => ({
  id: 41, function: "checkout", version: 3, trigger: "http", status: "ok", duration_ms: 18, fuel_used: 21_000, error: null, logs: [{ level: "info", message: "order 7 placed" }], request_id: "req-9", created_at: T0, trace_id: null,
  profile: {
    cold_start: true, prepare_us: 900, duration_us: 18_000, fuel_used: 21_000, fuel_limit: 1_000_000, memory_peak_bytes: 2_097_152, memory_limit_bytes: 67_108_864, tenant: "env1", host_calls: 3,
    host: { "db.query": { count: 2, total_us: 2500, max_us: 2000, errors: 1 }, "kv.get": { count: 1, total_us: 100, max_us: 100, errors: 0 } },
    timeline: [{ op: "db.query", at_us: 1000, dur_us: 500, ok: true }, { op: "kv.get", at_us: 2000, dur_us: 100, ok: true }, { op: "db.query", at_us: 5000, dur_us: 2000, ok: false }], timeline_dropped: 0,
  }, ...o,
});

export const audit = (o: Partial<InsightAuditEntry> = {}): InsightAuditEntry => ({
  id: 10, at: T0, event: "login", outcome: "success", actor: "user:users/42", resource_type: "session", collection: "users", resource_id: "42", ip: "203.0.113.9", user_agent: "Mozilla/5.0",
  request_id: "req-1", impersonator: null, detail: { method: "password" }, ...o,
});

export const usageRow = (o: Partial<UsageRow> = {}): UsageRow => ({
  level: "environment", id: "env1", name: "production", realm: "main", org: "acme", project: "shop", attributed: true,
  metrics: { "functions.invocations": { kind: "counter", value: 12_500, peak: null }, "storage.bytes": { kind: "gauge", value: 5_242_880, peak: 6_291_456 } },
  cost: { estimate: true, currency: "USD", amount_minor: 1230, amount: "12.30", credits: 40, lines: [{ metric: "functions.invocations", quantity: 12_500, price: "0.001", amount_minor: 1230, credits: 40 }] }, ...o,
});

export interface InsightsState {
  status: InsightsStatus;
  statements: StatementStat[];
  regressions: Regression[];
  slow: SlowStatement[];
  usage: UsageRow[];
  costAvailable: boolean;
  fnStats: FunctionStat[];
  invocations: Invocation[];
  audit: InsightAuditEntry[];
  related: { same_request: InsightAuditEntry[]; same_actor: InsightAuditEntry[] };
  flushes: number;
}

/** Install the routes on `mock` and return the state a test can change. Call after `signedIn`. */
export function installInsights(mock: MockApi, init: Partial<InsightsState> = {}): InsightsState {
  const st: InsightsState = {
    status: { enabled: true, slow_query_ms: 200, flush_interval_secs: 60, top_n: 200, retention_days: 14, fingerprints_in_memory: 37, unflushed_since: T0, stored_rows: 100, stored_slow_statements: 5 },
    statements: [stmt(), stmt({ fingerprint: "fp-jobs", statement: "update jobs set status = ? where id = ?", area: "jobs", calls: 90, errors: 0, p95_ms: 250, total_ms: 9000, writes: 90 })],
    regressions: [], slow: [], usage: [usageRow()], costAvailable: true, fnStats: [fnStat()], invocations: [invocation()], audit: [audit()],
    related: { same_request: [], same_actor: [] }, flushes: 0, ...init,
  };
  const I = "/api/v1/insights";
  const on = (method: string, path: string, h: (r: MockRequest) => { status: number; body?: unknown }) => mock.on(method, path, h);
  const series = (n: number) => Array.from({ length: n }, (_, i) => ({ at: `2026-11-14T0${i}:00:00Z`, seconds: 60, calls: 100 + i * 10, errors: i % 2, mean_ms: 4 + i, p95_ms: 10 + i * 3, max_ms: 50 }));

  on("GET", `${I}/status`, () => ({ status: 200, body: st.status }));
  on("POST", `${I}/flush`, () => { st.flushes++; return { status: 200, body: { statements: 37, slow_statements: 2, slow_dropped: 0, functions: 1 } }; });
  on("GET", `${I}/queries`, ({ query }) => {
    const area = query.get("area");
    const q = query.get("q");
    const items = st.statements.filter((s) => (!area || s.area === area) && (!q || s.statement.includes(q)));
    const offset = Number(query.get("offset") ?? 0);
    const limit = Number(query.get("limit") ?? 50);
    return { status: 200, body: { items: items.slice(offset, offset + limit), total: items.length, from: T0, to: T0, limit, offset } };
  });
  on("GET", `${I}/queries/regressions`, () => ({ status: 200, body: { recent_from: "2026-11-14T09:00:00Z", recent_to: T0, baseline_from: "2026-11-13T09:00:00Z", baseline_to: "2026-11-14T09:00:00Z", min_ratio: 2, min_calls: 20, min_delta_ms: 1, items: st.regressions } }));
  on("GET", `${I}/queries/{fingerprint}`, ({ params }) => {
    const s = st.statements.find((x) => x.fingerprint === params.fingerprint);
    return s ? { status: 200, body: { ...s, series: series(6) } } : envelope(404, "not_found", "no calls of that statement in the range");
  });
  on("GET", `${I}/slow-queries`, ({ query }) => {
    const cursor = query.get("cursor");
    const limit = Number(query.get("limit") ?? 50);
    const start = cursor ? st.slow.findIndex((s) => String(s.id) === cursor) + 1 : 0;
    const page = st.slow.slice(start, start + limit);
    return { status: 200, body: { items: page, next_cursor: start + limit < st.slow.length ? page.at(-1)!.id : null } };
  });
  on("GET", `${I}/usage/metrics`, () => ({ status: 200, body: { items: [
    { key: "functions.invocations", kind: "counter", unit: "calls", description: "Function executions", source: "functions" },
    { key: "storage.bytes", kind: "gauge", unit: "bytes", description: "Bytes of stored objects", source: "daily rollup" },
  ] } }));
  on("GET", `${I}/usage/top`, ({ query }) => {
    const n = Number(query.get("n") ?? 10);
    return { status: 200, body: { from: "2026-11-01T00:00:00Z", to: "2026-12-01T00:00:00Z", group_by: query.get("group_by") ?? "environment", sort: query.get("metric"), total: st.usage.length, limit: n, offset: 0, currency: st.costAvailable ? "USD" : null, cost_available: st.costAvailable, estimate: true, items: st.usage.slice(0, n).map((u) => (st.costAvailable ? u : { ...u, cost: null })) } };
  });
  on("GET", `${I}/usage/series`, ({ query }) => ({ status: 200, body: { scope: query.get("scope"), metrics: { "functions.invocations": [["2026-11-12", 100], ["2026-11-13", 140], ["2026-11-14", 90]], "storage.bytes": [["2026-11-12", 10], ["2026-11-13", 11], ["2026-11-14", 12]] } } }));
  on("GET", `${I}/functions`, ({ query }) => ({ status: 200, body: { items: st.fnStats.filter((f) => !query.get("function") || f.function === query.get("function")), from: T0, to: T0 } }));
  on("GET", `${I}/functions/invocations`, ({ query }) => {
    const cursor = query.get("cursor");
    const limit = Number(query.get("limit") ?? 50);
    const all = st.invocations.filter((i) => (!query.get("status") || i.status === query.get("status")) && (query.get("errors") !== "true" || i.status !== "ok") && (!query.get("function") || i.function === query.get("function")));
    const start = cursor ? all.findIndex((i) => String(i.id) === cursor) + 1 : 0;
    const page = all.slice(start, start + limit);
    return { status: 200, body: { items: page, next_cursor: start + limit < all.length ? page.at(-1)!.id : null } };
  });
  on("GET", `${I}/functions/invocations/{id}`, ({ params }) => {
    const i = st.invocations.find((x) => String(x.id) === params.id);
    return i ? { status: 200, body: i } : envelope(404, "not_found", "no such invocation");
  });
  on("GET", `${I}/functions/{name}`, ({ params }) => {
    const v = st.fnStats.filter((f) => f.function === params.name);
    return { status: 200, body: { function: params.name, versions: v, series: series(4).map((p) => ({ at: p.at, invocations: p.calls, errors: p.errors, cold_starts: 1, mean_ms: p.mean_ms, p95_ms: p.p95_ms })) } };
  });
  on("GET", `${I}/audit/summary`, () => ({ status: 200, body: { total: st.audit.length, outcomes: [{ key: "success", count: st.audit.length }, { key: "failure", count: 2 }], events: [], actors: [], ips: [] } }));
  on("GET", `${I}/audit`, ({ query }) => {
    const cursor = query.get("cursor");
    const limit = Number(query.get("limit") ?? 50);
    const all = st.audit.filter((e) => (!query.get("outcome") || e.outcome === query.get("outcome")) && (!query.get("actor") || e.actor === query.get("actor")) && (!query.get("action") || e.event.startsWith(query.get("action")!)));
    const start = cursor ? all.findIndex((e) => String(e.id) === cursor) + 1 : 0;
    const page = all.slice(start, start + limit);
    return { status: 200, body: { items: page, next_cursor: start + limit < all.length ? page.at(-1)!.id : null } };
  });
  on("GET", `${I}/audit/{id}/related`, ({ params }) => {
    const e = st.audit.find((x) => String(x.id) === params.id);
    return e ? { status: 200, body: { event: e, ...st.related } } : envelope(404, "not_found", "no such entry");
  });
  on("GET", `${I}/audit/{id}`, ({ params }) => {
    const e = st.audit.find((x) => String(x.id) === params.id);
    return e ? { status: 200, body: e } : envelope(404, "not_found", "no such entry");
  });
  return st;
}
