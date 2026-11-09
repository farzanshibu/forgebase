import { describe, expect, it } from "vitest";
import { ForgeBase } from "../src/index";
import { json, scripted } from "./helpers";

const decision = { allowed: true, feature: "ai", subject: "u", scope_key: "", units: 1, used: 0, reserved: 0, pack_remaining: 0, overage_units: 0, pack_units: 0, window_start: "t" };
const setup = (...r: Parameters<typeof scripted>) => {
  const f = scripted(...r);
  return { f, e: new ForgeBase("http://x", { fetch: f.fetch }).entitlements };
};

describe("entitlements", () => {
  it("has() sends exactly one selector", async () => {
    const { f, e } = setup(json(200, { has: true }), json(200, { has: false, reason: "no plan" }), json(200, { has: true }));
    expect(await e.has({ feature: "beta" })).toBe(true);
    expect(await e.hasDetailed({ permission: "org:billing:read" })).toEqual({ has: false, reason: "no plan" });
    expect(await e.has({ plan: "pro", subject: "org:1" })).toBe(true);
    expect(Object.fromEntries(new URL(f.calls[0]!.url).searchParams)).toEqual({ feature: "beta" });
    expect(Object.fromEntries(new URL(f.calls[1]!.url).searchParams)).toEqual({ permission: "org:billing:read" });
    expect(Object.fromEntries(new URL(f.calls[2]!.url).searchParams)).toEqual({ plan: "pro", subject: "org:1" });
  });

  it("maps quota requests to the wire format", async () => {
    const { f, e } = setup(json(200, decision), json(201, { event_id: "e", replayed: false, decision }));
    await e.checkQuota({ feature: "ai", units: 3, scopeKey: "team:1" });
    await e.consume({ feature: "ai", idempotencyKey: "k", subject: "org:9", metadata: { a: 1 } });
    expect(JSON.parse(f.calls[0]!.body!)).toEqual({ feature: "ai", units: 3, scope_key: "team:1" });
    expect(JSON.parse(f.calls[1]!.body!)).toEqual({ feature: "ai", idempotency_key: "k", subject: "org:9", metadata: { a: 1 } });
  });

  it("withReservation commits on success and releases on failure", async () => {
    const res = { id: "r1", status: "held" };
    const ok = setup(json(201, res), json(200, res));
    expect(await ok.e.withReservation({ feature: "ai", idempotencyKey: "k" }, async () => "done")).toBe("done");
    expect(ok.f.calls.map((c) => c.url.split("/v1")[1])).toEqual(["/usage/reservations", "/usage/reservations/r1/commit"]);
    const bad = setup(json(201, res), json(200, res));
    await expect(bad.e.withReservation({ feature: "ai", idempotencyKey: "k" }, async () => Promise.reject(new Error("fail")))).rejects.toThrow("fail");
    expect(bad.f.calls[1]!.url).toMatch(/release$/);
  });

  it("manages leases", async () => {
    const lease = { id: "l1" };
    const { f, e } = setup(json(201, lease), json(200, lease), json(204, null));
    await e.acquireLease({ feature: "jobs", idempotencyKey: "k", ttlSecs: 30 });
    await e.renewLease("l1", 60);
    await e.releaseLease("l1");
    expect(f.calls.map((c) => `${c.method} ${c.url.split("/v1")[1]}`)).toEqual(["POST /usage/leases", "POST /usage/leases/l1/renew", "DELETE /usage/leases/l1"]);
    expect(JSON.parse(f.calls[0]!.body!).ttl_secs).toBe(30);
  });
});
