import { describe, expect, it } from "vitest";
import { AuditExport, ForgeBase, PermissionError } from "../src/index";
import { errBody, json, noSleep, scripted } from "./helpers";

const client = (...replies: Parameters<typeof scripted>) => {
  const s = scripted(...replies);
  return { fb: new ForgeBase("http://x", { apiKey: "k", fetch: s.fetch, retry: { ...noSleep, maxRetries: 0 } }), calls: s.calls };
};
const q = (c: { url: string }) => {
  const u = new URL(c.url);
  return `${u.pathname.replace("/api/v1/insights", "")}${u.search ? "?" + decodeURIComponent(u.searchParams.toString()) : ""}`;
};
const D = new Date("2026-03-01T00:00:00Z");

describe("insights: statements", () => {
  it("status, flush, queries, one query, regressions, slow", async () => {
    const { fb, calls } = client(json(200, { enabled: true }), json(200, { buckets: 2 }), json(200, { items: [] }), json(200, { fingerprint: "abc" }), json(200, { items: [] }), json(200, { items: [], next_cursor: 5 }));
    expect((await fb.insights.status()) as unknown).toEqual({ enabled: true });
    await fb.insights.flush();
    await fb.insights.queries({ from: D, to: "2026-03-02", sort: "p95", area: "data", backend: "sqlite", q: "SELECT", limit: 10, offset: 20 });
    await fb.insights.query("abc/1", { from: "2026-03-01" });
    await fb.insights.regressions({ window: 600, baseline: 7200, until: D, minCalls: 5, minRatio: 1.5, minDeltaMs: 2, area: "queue" });
    await fb.insights.slow({ from: D, fingerprint: "abc", area: "data", minMs: 100, q: "x", cursor: 9, limit: 3 });
    expect(calls.map((c) => `${c.method} ${q(c)}`)).toEqual([
      "GET /status",
      "POST /flush",
      "GET /queries?from=2026-03-01T00:00:00.000Z&to=2026-03-02&sort=p95&area=data&backend=sqlite&q=SELECT&limit=10&offset=20",
      "GET /queries/abc%2F1?from=2026-03-01",
      "GET /queries/regressions?window=600&baseline=7200&until=2026-03-01T00:00:00.000Z&min_calls=5&min_ratio=1.5&min_delta_ms=2&area=queue",
      "GET /slow-queries?from=2026-03-01T00:00:00.000Z&fingerprint=abc&area=data&min_ms=100&q=x&cursor=9&limit=3",
    ]);
    expect(calls[1]!.body).toBe("{}");
  });
});

describe("insights: usage", () => {
  it("usage, top, series, metrics, rollup", async () => {
    const { fb, calls } = client(json(200, { rows: [] }), json(200, { rows: [] }), json(200, { points: [] }), json(200, { metrics: [] }), json(200, { day: "2026-03-01" }), json(200, { day: "2026-03-02" }));
    await fb.insights.usage({ period: "month", date: "2026-03", groupBy: "org", metric: "storage.bytes,usage.api.requests", sort: "cost", realm: "r", org: "o", project: "p", currency: "EUR", limit: 5, offset: 1 });
    await fb.insights.top("cost", { n: 3, period: "day", date: "2026-03-01", groupBy: "project", currency: "USD" });
    await fb.insights.series("env:abc", { from: "2026-02-01", to: D });
    await fb.insights.metrics();
    await fb.insights.rollup({ day: "2026-03-01" });
    await fb.insights.rollup();
    expect(calls.map((c) => `${c.method} ${q(c)}`)).toEqual([
      "GET /usage?period=month&date=2026-03&group_by=org&metric=storage.bytes,usage.api.requests&sort=cost&realm=r&org=o&project=p&currency=EUR&limit=5&offset=1",
      "GET /usage/top?metric=cost&n=3&period=day&date=2026-03-01&group_by=project&currency=USD",
      "GET /usage/series?scope=env:abc&from=2026-02-01&to=2026-03-01T00:00:00.000Z",
      "GET /usage/metrics",
      "POST /usage/rollup?day=2026-03-01",
      "POST /usage/rollup",
    ]);
  });
});

describe("insights: functions", () => {
  it("functions, one function, invocations, one invocation", async () => {
    const { fb, calls } = client(json(200, { items: [] }), json(200, { name: "hello" }), json(200, { items: [], next_cursor: null }), json(200, { id: "i1" }));
    await fb.insights.functions({ from: D, function: "hello", version: 2 });
    await fb.insights.function("hello", { to: "2026-03-02" });
    await fb.insights.invocations({ function: "hello", version: 2, status: "timeout", trigger: "http", requestId: "r1", errors: true, minDurationMs: 50, cursor: 7, limit: 20 });
    await fb.insights.invocation("i 1");
    expect(calls.map((c) => `${c.method} ${q(c)}`)).toEqual([
      "GET /functions?from=2026-03-01T00:00:00.000Z&function=hello&version=2",
      "GET /functions/hello?to=2026-03-02",
      "GET /functions/invocations?function=hello&version=2&status=timeout&trigger=http&request_id=r1&errors=true&min_duration_ms=50&cursor=7&limit=20",
      "GET /functions/invocations/i%201",
    ]);
  });
});

describe("insights: audit", () => {
  it("search, entry, related, summary", async () => {
    const { fb, calls } = client(json(200, { items: [], next_cursor: 3 }), json(200, { id: "e1" }), json(200, { same_request: [], same_actor: [] }), json(200, { total: 1 }));
    const page = await fb.insights.audit({
      actor: "u1", actorType: "admin", action: "api_key.", resourceType: "api_key", collection: "posts", resourceId: "k1", outcome: "failure", ip: "1.2.3.4",
      requestId: "r1", from: D, to: "2026-04-01", q: "login", order: "asc", cursor: 1, limit: 25,
    });
    expect(page.next_cursor).toBe(3);
    await fb.insights.auditEntry("e1");
    await fb.insights.auditRelated("e1", { windowSecs: 60, limit: 10 });
    await fb.insights.auditSummary({ from: "2026-03-01" });
    expect(calls.map((c) => `${c.method} ${q(c)}`)).toEqual([
      "GET /audit?from=2026-03-01T00:00:00.000Z&to=2026-04-01&actor=u1&actor_type=admin&action=api_key.&resource_type=api_key&collection=posts&resource_id=k1&outcome=failure&ip=1.2.3.4&request_id=r1&q=login&order=asc&cursor=1&limit=25",
      "GET /audit/e1",
      "GET /audit/e1/related?window_secs=60&limit=10",
      "GET /audit/summary?from=2026-03-01",
    ]);
  });

  const stream = (chunks: string[], headers: Record<string, string> = {}) =>
    new Response(
      new ReadableStream<Uint8Array>({
        start(c) {
          const enc = new TextEncoder();
          for (const ch of chunks) c.enqueue(enc.encode(ch));
          c.close();
        },
      }),
      { status: 200, headers },
    );

  it("exports CSV as lines across chunk boundaries (CRLF and a last line without a line end)", async () => {
    const { fb, calls } = client(stream(["id,actor\r\n1,a", "da\n2,é", "à\n3,end"], { "content-type": "text/csv", "content-disposition": 'attachment; filename="audit.csv"' }));
    const exp = await fb.insights.exportAudit({ actor: "u1", from: D }, { maxRows: 100 });
    expect(exp).toBeInstanceOf(AuditExport);
    expect(exp.contentType).toBe("text/csv");
    expect(exp.filename).toBe("audit.csv");
    const lines: string[] = [];
    for await (const l of exp) lines.push(l);
    expect(lines).toEqual(["id,actor", "1,ada", "2,éà", "3,end"]);
    expect(q(calls[0]!)).toBe("/audit/export?from=2026-03-01T00:00:00.000Z&actor=u1&format=csv&max_rows=100");
    expect(calls[0]!.headers.Accept).toBe("text/csv");
  });

  it("exports JSONL and parses records; text() and stream give the raw export", async () => {
    const rows = [{ id: "e1" }, { id: "e2" }];
    const body = rows.map((r) => JSON.stringify(r)).join("\n") + "\n";
    const { fb, calls } = client(stream([body.slice(0, 10), body.slice(10)], { "content-type": "application/x-ndjson" }), stream(["a\nb\n"]), stream(["xy"]));
    const exp = await fb.insights.exportAudit({}, { format: "jsonl" });
    const got: unknown[] = [];
    for await (const r of exp.records()) got.push(r);
    expect(got).toEqual(rows);
    expect(q(calls[0]!)).toBe("/audit/export?format=jsonl");
    expect(await (await fb.insights.exportAudit()).text()).toBe("a\nb\n");
    const bytes: number[] = [];
    const reader = (await fb.insights.exportAudit()).stream.getReader();
    for (let r = await reader.read(); !r.done; r = await reader.read()) bytes.push(...r.value);
    expect(new TextDecoder().decode(new Uint8Array(bytes))).toBe("xy");
  });

  it("an export refused up front is a typed error", async () => {
    const { fb } = client(json(403, errBody("forbidden", "needs engine:read_audit")));
    await expect(fb.insights.exportAudit()).rejects.toBeInstanceOf(PermissionError);
  });
});
