import { describe, expect, it } from "vitest";
import { ApiError, ConflictError, ForgeBase, PermissionError } from "../src/index";
import { CreditsApi } from "../src/credits";
import { errBody, json, scripted } from "./helpers";

const entry = (seq: number, over: Record<string, unknown> = {}) => ({
  id: `le_${seq}`, seq, subject: "usr_1", kind: "top_up", amount: 100, balance_after: 100, actor: "admin", at: "2025-01-01T00:00:00Z",
  idempotency_key: `k${seq}`, metadata: {}, ...over,
});

const setup = (...r: Parameters<typeof scripted>) => {
  const f = scripted(...r);
  return { f, c: new CreditsApi(new ForgeBase("http://x", { fetch: f.fetch, retry: { maxRetries: 0 } }).transport) };
};
const urls = (f: { calls: { method: string; url: string }[] }) => f.calls.map((c) => `${c.method} ${c.url}`);

describe("credits: balances and ledger", () => {
  it("balance and audit pass the subject", async () => {
    const { f, c } = setup(json(200, { subject: "org:1", balance: 40, expiring: [{ amount: 10, expires_at: "t" }] }), json(200, { balance: 40 }), json(200, { consistent: true }));
    expect((await c.balance({ subject: "org:1" })).balance).toBe(40);
    await c.balance();
    expect((await c.audit({ subject: "org:1" })).consistent).toBe(true);
    expect(urls(f)).toEqual([
      "GET http://x/api/v1/credits/balance?subject=org%3A1",
      "GET http://x/api/v1/credits/balance",
      "GET http://x/api/v1/credits/audit?subject=org%3A1",
    ]);
  });

  it("lots unwraps items and sends all=true only when asked", async () => {
    const { f, c } = setup(json(200, { items: [{ id: "lot1" }] }), json(200, { items: [] }));
    expect(await c.lots({ all: true, subject: "u" })).toEqual([{ id: "lot1" }]);
    await c.lots({ all: false });
    expect(urls(f)).toEqual(["GET http://x/api/v1/credits/lots?subject=u&all=true", "GET http://x/api/v1/credits/lots"]);
  });

  it("ledger maps every filter, Dates become RFC 3339", async () => {
    const { f, c } = setup(json(200, { items: [entry(1)], next: 1 }));
    const page = await c.ledger({ subject: "u", kind: "consumption", after: 7, from: new Date("2025-01-01T00:00:00Z"), to: "2025-02-01T00:00:00Z", limit: 50, all: true });
    expect(page.next).toBe(1);
    expect(f.calls[0]!.url).toBe(
      "http://x/api/v1/credits/ledger?subject=u&all=true&kind=consumption&after=7&from=2025-01-01T00%3A00%3A00.000Z&to=2025-02-01T00%3A00%3A00Z&limit=50",
    );
  });

  it("entries follows `next` until the last page", async () => {
    const { f, c } = setup(
      json(200, { items: [entry(1), entry(2)], next: 2 }),
      json(200, { items: [entry(3)], next: null }),
    );
    const seen: number[] = [];
    for await (const e of c.entries({ limit: 2 })) seen.push(e.seq);
    expect(seen).toEqual([1, 2, 3]);
    expect(f.calls[0]!.url).toBe("http://x/api/v1/credits/ledger?limit=2");
    expect(f.calls[1]!.url).toBe("http://x/api/v1/credits/ledger?after=2&limit=2");
  });

  it("entry encodes the id; unknown is a NotFound-style ApiError", async () => {
    const { f, c } = setup(json(200, entry(1)), json(404, errBody("not_found")));
    await c.entry("le/1");
    expect(f.calls[0]!.url).toBe("http://x/api/v1/credits/ledger/le%2F1");
    await expect(c.entry("nope")).rejects.toMatchObject({ status: 404 });
  });
});

describe("credits: mutations", () => {
  const posted = (replayed: boolean, over = {}) => json(replayed ? 200 : 201, { entry: entry(1, over), replayed });

  it("topUp and grant send snake_case bodies with the idempotency key", async () => {
    const { f, c } = setup(posted(false), posted(false, { kind: "grant" }));
    const t = await c.topUp({ subject: "org:1", amount: 500, idempotencyKey: "pay_1", externalRef: "pi_9", reason: "pack", metadata: { sku: "x" } });
    expect(t.replayed).toBe(false);
    await c.grant({ subject: "org:1", amount: 50, idempotencyKey: "g1", expiresAt: new Date("2026-01-01T00:00:00Z") });
    expect(f.calls[0]).toMatchObject({ method: "POST", url: "http://x/api/v1/credits/top-ups" });
    expect(JSON.parse(f.calls[0]!.body!)).toEqual({ subject: "org:1", amount: 500, idempotency_key: "pay_1", external_ref: "pi_9", reason: "pack", metadata: { sku: "x" } });
    expect(f.calls[1]).toMatchObject({ method: "POST", url: "http://x/api/v1/credits/grants" });
    expect(JSON.parse(f.calls[1]!.body!)).toEqual({ subject: "org:1", amount: 50, idempotency_key: "g1", expires_at: "2026-01-01T00:00:00.000Z" });
  });

  it("a replayed key comes back as replayed: true", async () => {
    const { c } = setup(posted(true));
    expect((await c.topUp({ subject: "u", amount: 1, idempotencyKey: "k" })).replayed).toBe(true);
  });

  it("topUp by a non-operator is a PermissionError", async () => {
    const { c } = setup(json(403, errBody("forbidden")));
    await expect(c.topUp({ subject: "u", amount: 1, idempotencyKey: "k" })).rejects.toBeInstanceOf(PermissionError);
  });

  it("spend maps feature and units; 402 insufficient_credits keeps the balance in details", async () => {
    const { f, c } = setup(posted(false, { kind: "spend", amount: -5 }), json(402, errBody("insufficient_credits", "not enough", { balance: 2, requested: 5 })));
    await c.spend({ amount: 5, idempotencyKey: "s1", feature: "ai.tokens", units: 1000, subject: "org:1" });
    expect(f.calls[0]).toMatchObject({ method: "POST", url: "http://x/api/v1/credits/spend" });
    expect(JSON.parse(f.calls[0]!.body!)).toEqual({ amount: 5, idempotency_key: "s1", subject: "org:1", feature: "ai.tokens", units: 1000 });
    const err = await c.spend({ amount: 5, idempotencyKey: "s2" }).catch((e: unknown) => e);
    expect(err).toBeInstanceOf(ApiError);
    expect(err).toMatchObject({ status: 402, code: "insufficient_credits", details: { balance: 2, requested: 5 } });
  });

  it("refund and reverse take an entry id; amount is optional", async () => {
    const { f, c } = setup(posted(false, { kind: "refund" }), json(200, { reversed: 30, unrecovered: 20, replayed: false, entry: entry(2) }), json(200, { reversed: 0, unrecovered: 0, replayed: false }));
    await c.refund({ entryId: "le_9", idempotencyKey: "r1", amount: 3, reason: "oops" });
    const rev = await c.reverse({ entryId: "le_1", idempotencyKey: "v1" });
    expect(rev).toMatchObject({ reversed: 30, unrecovered: 20 });
    expect((await c.reverse({ entryId: "le_1", idempotencyKey: "v2" })).reversed).toBe(0);
    expect(f.calls[0]).toMatchObject({ url: "http://x/api/v1/credits/refunds" });
    expect(JSON.parse(f.calls[0]!.body!)).toEqual({ entry_id: "le_9", idempotency_key: "r1", amount: 3, reason: "oops" });
    expect(f.calls[1]).toMatchObject({ method: "POST", url: "http://x/api/v1/credits/reversals" });
    expect(JSON.parse(f.calls[1]!.body!)).toEqual({ entry_id: "le_1", idempotency_key: "v1" });
  });

  it("an idempotency key reused with other content is a ConflictError", async () => {
    const { c } = setup(json(409, errBody("idempotency_conflict")));
    await expect(c.spend({ amount: 1, idempotencyKey: "k" })).rejects.toBeInstanceOf(ConflictError);
  });

  it("maintenance POSTs without a body", async () => {
    const report = { expired: { credits: 5, lots: 1 }, usage: { credits_charged: 0, cursor: 3, events_charged: 0, events_read: 3, shortfall: 0 } };
    const { f, c } = setup(json(200, report));
    expect(await c.maintenance()).toEqual(report);
    expect(f.calls[0]).toMatchObject({ method: "POST", url: "http://x/api/v1/credits/maintenance" });
    expect(f.calls[0]!.body).toBeUndefined();
  });
});

describe("credits: rates, usage and stats", () => {
  it("rates list / set / delete", async () => {
    const rate = { feature: "ai.tokens", credits_per_unit: 2, per_units: 1000, basis: "units", created_at: "t", updated_at: "t" };
    const { f, c } = setup(json(200, { items: [rate] }), json(200, rate), json(204, null));
    expect(await c.rates.list()).toEqual([rate]);
    expect(await c.rates.set("ai.tokens", { creditsPerUnit: 2, perUnits: 1000, basis: "units" })).toEqual(rate);
    await c.rates.delete("ai.tokens");
    expect(urls(f)).toEqual(["GET http://x/api/v1/credits/rates", "PUT http://x/api/v1/credits/rates/ai.tokens", "DELETE http://x/api/v1/credits/rates/ai.tokens"]);
    expect(JSON.parse(f.calls[1]!.body!)).toEqual({ credits_per_unit: 2, per_units: 1000, basis: "units" });
  });

  it("rates.set leaves optional fields out", async () => {
    const { f, c } = setup(json(200, {}));
    await c.rates.set("a b", { creditsPerUnit: 1 });
    expect(f.calls[0]!.url).toBe("http://x/api/v1/credits/rates/a%20b");
    expect(JSON.parse(f.calls[0]!.body!)).toEqual({ credits_per_unit: 1 });
  });

  it("usage maps its filters", async () => {
    const { f, c } = setup(json(200, { items: [], next: null }));
    await c.usage({ subject: "org:1", shortfall: true, after: 10, limit: 20, all: true });
    expect(f.calls[0]!.url).toBe("http://x/api/v1/credits/usage?subject=org%3A1&all=true&shortfall=true&after=10&limit=20");
  });

  it("usageCursor get / set return the seq", async () => {
    const { f, c } = setup(json(200, { seq: 12 }), json(200, { seq: 0 }));
    expect(await c.usageCursor.get()).toBe(12);
    expect(await c.usageCursor.set(0)).toBe(0);
    expect(f.calls[1]).toMatchObject({ method: "PUT", url: "http://x/api/v1/credits/usage/cursor" });
    expect(JSON.parse(f.calls[1]!.body!)).toEqual({ seq: 0 });
  });

  it("stats.balance / consumption / summary build their windows", async () => {
    const { f, c } = setup(json(200, { points: [] }), json(200, { rows: [] }), json(200, { balance: 1 }), json(200, {}));
    const from = new Date("2025-03-01T00:00:00Z");
    await c.stats.balance({ subject: "u", from, to: "2025-04-01T00:00:00Z", interval: "month" });
    await c.stats.consumption({ all: true, interval: "hour" });
    await c.stats.summary({ subject: "org:1", from, all: true });
    await c.stats.summary();
    expect(urls(f)).toEqual([
      "GET http://x/api/v1/credits/stats/balance?subject=u&from=2025-03-01T00%3A00%3A00.000Z&to=2025-04-01T00%3A00%3A00Z&interval=month",
      "GET http://x/api/v1/credits/stats/consumption?all=true&interval=hour",
      "GET http://x/api/v1/credits/stats/summary?subject=org%3A1&all=true&from=2025-03-01T00%3A00%3A00.000Z",
      "GET http://x/api/v1/credits/stats/summary",
    ]);
  });

  it("an invalid parameter is a ValidationError-class 400", async () => {
    const { c } = setup(json(400, errBody("invalid_parameter", "interval must be hour, day or month")));
    await expect(c.stats.balance({ interval: "week" as never })).rejects.toMatchObject({ status: 400, code: "invalid_parameter" });
  });
});
