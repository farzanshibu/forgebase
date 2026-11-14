import { describe, expect, it } from "vitest";
import { ForgeBase, NotFoundError } from "../src/index";
import { errBody, json, noSleep, scripted } from "./helpers";

const client = (...replies: Parameters<typeof scripted>) => {
  const s = scripted(...replies);
  return { fb: new ForgeBase("http://x", { apiKey: "k", fetch: s.fetch, retry: { ...noSleep, maxRetries: 0 } }), calls: s.calls };
};
const body = (c: { body?: string }) => JSON.parse(c.body ?? "null") as unknown;
const line = (c: { method: string; url: string }) => `${c.method} ${c.url.replace("http://x", "")}`;

describe("alerts", () => {
  it("lists active alerts, history, one alert and notifications", async () => {
    const { fb, calls } = client(json(200, [{ id: "a1" }]), json(200, { items: [{ id: "a1" }], next_before: "c1" }), json(200, { alert: { id: "a1" }, events: [] }), json(200, { items: [] }));
    expect(await fb.alerts.active({ state: "firing", severity: "critical" })).toEqual([{ id: "a1" }]);
    const h = await fb.alerts.history({ state: "resolved", ruleId: "r1", from: new Date("2026-01-01T00:00:00Z"), to: "2026-02-01", before: "c0", limit: 10 });
    expect(h.next_before).toBe("c1");
    await fb.alerts.get("a 1");
    await fb.alerts.notifications({ notifier: "pager", status: "failed", limit: 5 });
    expect(calls.map(line)).toEqual([
      "GET /api/v1/alerts?state=firing&severity=critical",
      "GET /api/v1/alerts/history?state=resolved&rule_id=r1&from=2026-01-01T00%3A00%3A00.000Z&to=2026-02-01&before=c0&limit=10",
      "GET /api/v1/alerts/a%201",
      "GET /api/v1/alerts/notifications?notifier=pager&status=failed&limit=5",
    ]);
  });

  it("evaluates an expression", async () => {
    const { fb, calls } = client(json(200, { samples: [{ labels: {}, value: 1 }] }));
    await fb.alerts.query("forgebase_up == 0");
    expect(line(calls[0]!)).toBe("POST /api/v1/alerts/query");
    expect(body(calls[0]!)).toEqual({ expr: "forgebase_up == 0" });
  });

  it("rule CRUD", async () => {
    const rule = { id: "r1", name: "high-latency", expr: "x", threshold: 1 };
    const { fb, calls } = client(json(200, [rule]), json(201, rule), json(200, rule), json(200, { ...rule, enabled: false }), json(204, null));
    const r = fb.alerts.rules;
    expect(await r.list()).toEqual([rule]);
    await r.create({ name: "high-latency", expr: "x", threshold: 1, for: "5m", severity: "warning", op: ">" });
    await r.get("high-latency");
    expect((await r.update("r1", { enabled: false })) as { enabled: boolean }).toMatchObject({ enabled: false });
    expect(await r.delete("r1")).toBeUndefined();
    expect(calls.map(line)).toEqual(["GET /api/v1/alerts/rules", "POST /api/v1/alerts/rules", "GET /api/v1/alerts/rules/high-latency", "PATCH /api/v1/alerts/rules/r1", "DELETE /api/v1/alerts/rules/r1"]);
    expect(body(calls[1]!)).toEqual({ name: "high-latency", expr: "x", threshold: 1, for: "5m", severity: "warning", op: ">" });
    expect(body(calls[3]!)).toEqual({ enabled: false });
  });

  it("silences: list (all), create, get, replace, expire", async () => {
    const s = { id: "s1", matchers: [{ label: "alertname", op: "=", value: "x" }] };
    const { fb, calls } = client(json(200, [s]), json(201, s), json(200, s), json(200, s), json(200, s));
    const si = fb.alerts.silences;
    await si.list();
    await si.list({ all: true });
    await si.create({ matchers: s.matchers, duration: "1h", comment: "deploy" });
    await si.get("s1");
    await si.update("s1", { matchers: s.matchers, duration: "2h" });
    await si.expire("s1");
    expect(calls.map(line)).toEqual([
      "GET /api/v1/alerts/silences",
      "GET /api/v1/alerts/silences?all=true",
      "POST /api/v1/alerts/silences",
      "GET /api/v1/alerts/silences/s1",
      "PUT /api/v1/alerts/silences/s1",
      "DELETE /api/v1/alerts/silences/s1",
    ]);
  });

  it("notifiers: CRUD and test send (not retried)", async () => {
    const n = { id: "n1", name: "pager", kind: "webhook" };
    const delivery = { id: "d1", notifier_id: "n1", status: "sent" };
    const { fb, calls } = client(json(200, [n]), json(201, n), json(200, n), json(200, n), json(204, null), json(200, delivery));
    const no = fb.alerts.notifiers;
    await no.list();
    await no.create({ name: "pager", kind: "webhook", url: "https://e.test/h" } as never);
    await no.get("pager");
    await no.update("n1", { name: "pager", kind: "webhook", url: "https://e.test/h2" } as never);
    await no.delete("n1");
    expect((await no.test("n1")).status).toBe("sent");
    expect(calls.map(line)).toEqual([
      "GET /api/v1/alerts/notifiers", "POST /api/v1/alerts/notifiers", "GET /api/v1/alerts/notifiers/pager", "PUT /api/v1/alerts/notifiers/n1",
      "DELETE /api/v1/alerts/notifiers/n1", "POST /api/v1/alerts/notifiers/n1/test",
    ]);
  });

  it("a disabled feature is a NotFoundError", async () => {
    const { fb } = client(json(404, errBody("feature_disabled", "alerts are off")));
    await expect(fb.alerts.rules.list()).rejects.toBeInstanceOf(NotFoundError);
  });
});

describe("slos", () => {
  it("CRUD and status", async () => {
    const slo = { id: "slo1", name: "api-availability", kind: "availability", objective: 99.9 };
    const status = { slo: slo, compliance: 100 };
    const { fb, calls } = client(json(200, [slo]), json(201, slo), json(200, slo), json(200, slo), json(204, null), json(200, status), json(200, [status]));
    await fb.slos.list();
    await fb.slos.create({ name: "api-availability", kind: "availability", objective: 99.9, window: "30d" } as never);
    await fb.slos.get("slo1");
    await fb.slos.update("slo1", { name: "api-availability", kind: "availability", objective: 99.5 } as never);
    await fb.slos.delete("slo1");
    await fb.slos.status("slo1");
    expect(await fb.slos.statuses()).toEqual([status]);
    expect(calls.map(line)).toEqual([
      "GET /api/v1/slos", "POST /api/v1/slos", "GET /api/v1/slos/slo1", "PUT /api/v1/slos/slo1", "DELETE /api/v1/slos/slo1",
      "GET /api/v1/slos/slo1/status", "GET /api/v1/slos/status",
    ]);
  });
});
