import { describe, expect, it } from "vitest";
import { ConflictError, ForgeBase, NotFoundError, PermissionError, QuotaExceededError } from "../src/index";
import { errBody, json, scripted } from "./helpers";

const setup = (...r: Parameters<typeof scripted>) => {
  const f = scripted(...r);
  return { f, e: new ForgeBase("http://x", { fetch: f.fetch }).entitlements };
};
const body = (c: { body?: string }) => JSON.parse(c.body!) as unknown;
const route = (c: { method: string; url: string }) => `${c.method} ${c.url.replace("http://x", "")}`;

describe("entitlements.plans", () => {
  it("list, get, put, delete", async () => {
    const plan = { key: "pro", features: [] };
    const { f, e } = setup(json(200, { items: [plan] }), json(200, plan), json(200, plan), json(204, null));
    expect(await e.plans.list()).toEqual([plan]);
    expect(await e.plans.get("pro")).toEqual(plan);
    await e.plans.put("pro", { name: "Pro", isDefault: false, trialDays: 14, features: [{ feature: "seats", limit: 10, period: "none" }] });
    await expect(e.plans.delete("pro")).resolves.toBeUndefined();
    expect(f.calls.map(route)).toEqual(["GET /api/v1/plans", "GET /api/v1/plans/pro", "PUT /api/v1/plans/pro", "DELETE /api/v1/plans/pro"]);
    expect(body(f.calls[2]!)).toEqual({ name: "Pro", is_default: false, trial_days: 14, features: [{ feature: "seats", limit: 10, period: "none" }] });
  });

  it("a plan in use cannot be deleted; non-admins are a PermissionError", async () => {
    const { e } = setup(json(409, errBody("plan_in_use")), json(403, errBody("forbidden")));
    await expect(e.plans.delete("pro")).rejects.toBeInstanceOf(ConflictError);
    await expect(e.plans.put("pro", {})).rejects.toBeInstanceOf(PermissionError);
  });
});

describe("entitlements.features", () => {
  it("list, get, put, delete", async () => {
    const feat = { key: "api_calls", kind: "metered" };
    const { f, e } = setup(json(200, { items: [feat] }), json(200, feat), json(200, feat), json(204, null));
    expect(await e.features.list()).toEqual([feat]);
    expect(await e.features.get("api_calls")).toEqual(feat);
    await e.features.put("api_calls", { kind: "metered", name: "API calls", unit: "call", permissions: ["api:call"] });
    await e.features.delete("api_calls");
    expect(f.calls.map(route)).toEqual([
      "GET /api/v1/features",
      "GET /api/v1/features/api_calls",
      "PUT /api/v1/features/api_calls",
      "DELETE /api/v1/features/api_calls",
    ]);
    expect(body(f.calls[2]!)).toEqual({ kind: "metered", name: "API calls", unit: "call", permissions: ["api:call"] });
  });

  it("an unknown feature is a NotFoundError", async () => {
    const { e } = setup(json(404, errBody("not_found")));
    await expect(e.features.get("nope")).rejects.toBeInstanceOf(NotFoundError);
  });
});

describe("entitlements.addons and usagePacks", () => {
  it("addons: list, put, delete", async () => {
    const { f, e } = setup(json(200, { items: [{ key: "seats_5" }] }), json(200, { key: "seats_5" }), json(204, null));
    expect(await e.addons.list()).toEqual([{ key: "seats_5" }]);
    await e.addons.put("seats_5", { name: "5 seats", features: [{ feature: "seats", limit: 5 }] });
    await e.addons.delete("seats_5");
    expect(f.calls.map(route)).toEqual(["GET /api/v1/addons", "PUT /api/v1/addons/seats_5", "DELETE /api/v1/addons/seats_5"]);
    expect(body(f.calls[1]!)).toEqual({ name: "5 seats", features: [{ feature: "seats", limit: 5 }] });
  });

  it("usagePacks catalog: list, put, delete", async () => {
    const { f, e } = setup(json(200, { items: [{ key: "boost" }] }), json(200, { key: "boost" }), json(204, null));
    expect(await e.usagePacks.list()).toEqual([{ key: "boost" }]);
    await e.usagePacks.put("boost", { feature: "api_calls", units: 10000, validDays: 30 });
    await e.usagePacks.delete("boost");
    expect(f.calls.map(route)).toEqual(["GET /api/v1/usage-packs", "PUT /api/v1/usage-packs/boost", "DELETE /api/v1/usage-packs/boost"]);
    expect(body(f.calls[1]!)).toEqual({ feature: "api_calls", units: 10000, valid_days: 30 });
  });

  it("usagePacks held: grants of a subject and grant", async () => {
    const g = { id: "pg_1", remaining: 10000 };
    const { f, e } = setup(json(200, { items: [g] }), json(201, g));
    expect(await e.usagePacks.grants("org:1")).toEqual([g]);
    expect(await e.usagePacks.grant({ subject: "org:1", pack: "boost", quantity: 2, idempotencyKey: "order-9" })).toEqual(g);
    expect(f.calls.map(route)).toEqual(["GET /api/v1/entitlements/usage-packs?subject=org%3A1", "POST /api/v1/entitlements/usage-packs"]);
    expect(body(f.calls[1]!)).toEqual({ subject: "org:1", pack: "boost", quantity: 2, idempotency_key: "order-9" });
  });
});

describe("entitlements.quotaPolicies", () => {
  it("list, put, delete", async () => {
    const { f, e } = setup(json(200, { items: [{ feature: "api_calls" }] }), json(200, { feature: "api_calls" }), json(204, null));
    expect(await e.quotaPolicies.list()).toEqual([{ feature: "api_calls" }]);
    await e.quotaPolicies.put("api_calls", { enforcement: "soft", scope: "user", overagePct: 10, rolloverPct: 50, rolloverCap: 1000, leaseTtlSecs: 60, cost: 2 });
    await e.quotaPolicies.delete("api_calls");
    expect(f.calls.map(route)).toEqual(["GET /api/v1/quota-policies", "PUT /api/v1/quota-policies/api_calls", "DELETE /api/v1/quota-policies/api_calls"]);
    expect(body(f.calls[1]!)).toEqual({ enforcement: "soft", scope: "user", overage_pct: 10, rollover_pct: 50, rollover_cap: 1000, lease_ttl_secs: 60, cost: 2 });
  });
});

describe("entitlements.overrides and limits", () => {
  it("overrides: list by subject, add, delete", async () => {
    const o = { id: "ov_1", effect: "deny" };
    const { f, e } = setup(json(200, { items: [o] }), json(201, o), json(204, null));
    expect(await e.overrides.list("org:1")).toEqual([o]);
    expect(await e.overrides.add({ subject: "org:1", feature: "sso", effect: "allow", reason: "pilot", expiresAt: "2030-01-01T00:00:00Z", windowSecs: 60 })).toEqual(o);
    await e.overrides.delete("ov_1");
    expect(f.calls.map(route)).toEqual([
      "GET /api/v1/entitlements/overrides?subject=org%3A1",
      "POST /api/v1/entitlements/overrides",
      "DELETE /api/v1/entitlements/overrides/ov_1",
    ]);
    expect(body(f.calls[1]!)).toEqual({ subject: "org:1", feature: "sso", effect: "allow", reason: "pilot", expires_at: "2030-01-01T00:00:00Z", window_secs: 60 });
  });

  it("limits: list, put, delete send subject and feature as query", async () => {
    const { f, e } = setup(json(200, { items: [] }), json(200, { subject: "org:1", feature: "seats", limit: 500 }), json(204, null));
    await e.limits.list();
    expect(await e.limits.put({ subject: "org:1", feature: "seats", limit: 500, period: "none", reason: "enterprise" })).toMatchObject({ limit: 500 });
    await e.limits.delete("org:1", "seats");
    expect(f.calls.map(route)).toEqual([
      "GET /api/v1/entitlements/limits",
      "PUT /api/v1/entitlements/limits",
      "DELETE /api/v1/entitlements/limits?subject=org%3A1&feature=seats",
    ]);
    expect(body(f.calls[1]!)).toEqual({ subject: "org:1", feature: "seats", limit: 500, period: "none", reason: "enterprise" });
  });
});

describe("entitlements subscriptions, trials, add-ons", () => {
  const sub = { subject: "org:1", plan: "pro", status: "active", addons: [] };

  it("subscription reads one subject; none is a NotFoundError", async () => {
    const { f, e } = setup(json(200, sub), json(404, errBody("not_found", "no subscription")));
    expect(await e.subscription("org:1")).toEqual(sub);
    await expect(e.subscription()).rejects.toBeInstanceOf(NotFoundError);
    expect(f.calls[0]!.url).toBe("http://x/api/v1/entitlements/subscription?subject=org%3A1");
    expect(f.calls[1]!.url).toBe("http://x/api/v1/entitlements/subscription");
  });

  it("syncSubscription maps the idempotency and ordering fields", async () => {
    const { f, e } = setup(json(200, { subject: "org:1", plan: "pro", status: "active", result: "applied" }));
    expect((await e.syncSubscription({ subject: "org:1", plan: "pro", status: "active", eventId: "evt_1", eventAt: "2026-01-01T00:00:00Z", currentPeriodEnd: "2026-02-01T00:00:00Z", addons: [{ addon: "seats_5", quantity: 1 }] })).result).toBe("applied");
    expect(f.calls[0]).toMatchObject({ method: "PUT", url: "http://x/api/v1/entitlements/subscription" });
    expect(body(f.calls[0]!)).toEqual({
      subject: "org:1",
      plan: "pro",
      status: "active",
      event_id: "evt_1",
      event_at: "2026-01-01T00:00:00Z",
      current_period_end: "2026-02-01T00:00:00Z",
      addons: [{ addon: "seats_5", quantity: 1 }],
    });
  });

  it("startTrial and setAddon", async () => {
    const { f, e } = setup(json(201, { ...sub, status: "trialing" }), json(200, sub));
    expect((await e.startTrial({ subject: "org:1", plan: "pro", days: 14 })).status).toBe("trialing");
    await e.setAddon({ subject: "org:1", addon: "seats_5", quantity: 0 });
    expect(f.calls[0]).toMatchObject({ method: "POST", url: "http://x/api/v1/entitlements/trials" });
    expect(body(f.calls[0]!)).toEqual({ subject: "org:1", plan: "pro", days: 14 });
    expect(f.calls[1]).toMatchObject({ method: "PUT", url: "http://x/api/v1/entitlements/addons" });
    expect(body(f.calls[1]!)).toEqual({ subject: "org:1", addon: "seats_5", quantity: 0 });
  });
});

describe("entitlements usage admin", () => {
  it("adjustGauge sends delta or value; past the limit is a QuotaExceededError", async () => {
    const { f, e } = setup(json(201, { replayed: false, event_id: "ev_1" }), json(429, errBody("quota_exceeded", "over", { allowed: false })));
    expect(await e.adjustGauge({ feature: "seats", delta: 1, scopeKey: "team:7", idempotencyKey: "k1" })).toMatchObject({ event_id: "ev_1" });
    await expect(e.adjustGauge({ feature: "seats", value: 999 })).rejects.toBeInstanceOf(QuotaExceededError);
    expect(f.calls[0]).toMatchObject({ method: "POST", url: "http://x/api/v1/usage/gauge" });
    expect(body(f.calls[0]!)).toEqual({ feature: "seats", delta: 1, scope_key: "team:7", idempotency_key: "k1" });
    expect(body(f.calls[1]!)).toEqual({ feature: "seats", value: 999 });
  });

  it("usageEvents pages with after and limit", async () => {
    const { f, e } = setup(json(200, { items: [{ seq: 6 }], next: 6 }));
    expect(await e.usageEvents({ subject: "org:1", feature: "api_calls", after: 5, limit: 100 })).toEqual({ items: [{ seq: 6 }], next: 6 });
    await e.usageEvents();
    expect(f.calls[0]!.url).toBe("http://x/api/v1/usage/events?subject=org%3A1&feature=api_calls&after=5&limit=100");
    expect(f.calls[1]!.url).toBe("http://x/api/v1/usage/events");
  });

  it("usageAlerts unwraps items", async () => {
    const { f, e } = setup(json(200, { items: [{ id: "al_1", threshold: 90 }] }));
    expect(await e.usageAlerts("org:1")).toEqual([{ id: "al_1", threshold: 90 }]);
    expect(f.calls[0]!.url).toBe("http://x/api/v1/usage/alerts?subject=org%3A1");
  });

  it("maintenance posts without a body", async () => {
    const { f, e } = setup(json(200, { leases_expired: 1 }));
    expect(await e.maintenance()).toEqual({ leases_expired: 1 });
    expect(f.calls[0]).toMatchObject({ method: "POST", url: "http://x/api/v1/usage/maintenance" });
    expect(f.calls[0]!.body).toBeUndefined();
  });
});
