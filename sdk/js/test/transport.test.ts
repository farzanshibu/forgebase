import { describe, expect, it } from "vitest";
import { ForgeBase, QuotaExceededError, RateLimitError, ServerError } from "../src/index";
import { errBody, json, scripted } from "./helpers";

const make = (f: ReturnType<typeof scripted>, retry = {}) => {
  const sleeps: number[] = [];
  const fb = new ForgeBase("http://x.test/", {
    fetch: f.fetch,
    retry: { sleep: (ms) => (sleeps.push(ms), Promise.resolve()), random: () => 0.5, ...retry },
  });
  return { fb, sleeps };
};

describe("quota 429", () => {
  it("is never retried: waiting will not help", async () => {
    const f = scripted(json(429, errBody("quota_exceeded", "monthly quota used", { limit: 10 }), { "retry-after": "1" }), json(200, { ok: true }));
    const { fb, sleeps } = make(f);
    const err = await fb.request("/api/v1/x").catch((e: unknown) => e);
    expect(err).toBeInstanceOf(QuotaExceededError);
    expect(err).toMatchObject({ status: 429, code: "quota_exceeded", details: { limit: 10 } });
    expect(f.calls).toHaveLength(1);
    expect(sleeps).toEqual([]);
  });

  it("covers the other quota codes and POST", async () => {
    for (const code of ["usage_limit_reached", "entitlement_limit_exceeded"]) {
      const f = scripted(json(429, errBody(code)), json(200, {}));
      await expect(make(f).fb.request("/x", { method: "POST", body: {} })).rejects.toBeInstanceOf(QuotaExceededError);
      expect(f.calls).toHaveLength(1);
    }
  });

  it("a rate-limit 429 is still retried, with its body intact when the budget runs out", async () => {
    const f = scripted(json(429, errBody("rate_limited"), { "retry-after": "2" }), json(200, { ok: 1 }));
    const { fb, sleeps } = make(f);
    expect(await fb.request("/x")).toEqual({ ok: 1 });
    expect(sleeps).toEqual([2000]);
    const g = scripted(json(429, errBody("rate_limited", "slow down")));
    const err = await make(g, { maxRetries: 1 }).fb.request("/x").catch((e: unknown) => e);
    expect(err).toBeInstanceOf(RateLimitError);
    expect(err).toMatchObject({ message: "slow down" });
    expect(g.calls).toHaveLength(2);
  });
});

describe("acceptStatus", () => {
  it("resolves with the Response for the listed statuses and still throws for others", async () => {
    const { fb } = make(scripted(json(404, { gone: true })));
    const res = await fb.request<Response>("/x", { raw: true, acceptStatus: [404] });
    expect(res.status).toBe(404);
    expect(await res.json()).toEqual({ gone: true });
    const { fb: fb2 } = make(scripted(json(500, errBody("internal"))), { maxRetries: 0 });
    await expect(fb2.request("/x", { raw: true, acceptStatus: [404] })).rejects.toBeInstanceOf(ServerError);
    expect((await fb2.request<Response>("/x", { raw: true, acceptStatus: "any" })).status).toBe(500);
  });

  it("retries first: a 503 that recovers resolves normally", async () => {
    const f = scripted(json(503, errBody("u")), json(200, { ok: true }));
    const { fb } = make(f);
    expect((await fb.request<Response>("/x", { raw: true, acceptStatus: "any" })).status).toBe(200);
    expect(f.calls).toHaveLength(2);
  });
});

describe("conditional reads", () => {
  it("sends If-None-Match and answers a 304 with the remembered value", async () => {
    const f = scripted(json(200, { v: 1 }, { etag: '"a"' }), new Response(null, { status: 304, headers: { etag: '"a"' } }), json(200, { v: 2 }, { etag: '"b"' }));
    const { fb } = make(f);
    expect(await fb.request("/r", { conditional: true })).toEqual({ v: 1 });
    expect(f.calls[0]!.headers["If-None-Match"]).toBeUndefined();
    expect(await fb.request("/r", { conditional: true })).toEqual({ v: 1 });
    expect(f.calls[1]!.headers["If-None-Match"]).toBe('"a"');
    expect(await fb.request("/r", { conditional: true })).toEqual({ v: 2 });
    expect(f.calls[2]!.headers["If-None-Match"]).toBe('"a"');
  });

  it("is per URL and per opt-in; a server that ignores it just answers 200", async () => {
    const f = scripted(json(200, { n: 1 }, { etag: '"x"' }), json(200, { n: 2 }));
    const { fb } = make(f);
    await fb.request("/a", { conditional: true });
    expect(await fb.request("/b", { conditional: true })).toEqual({ n: 2 });
    expect(f.calls[1]!.headers["If-None-Match"]).toBeUndefined();
    await fb.request("/a");
    expect(f.calls[2]!.headers["If-None-Match"]).toBeUndefined();
  });

  it("clearConditionalCache forgets everything", async () => {
    const f = scripted(json(200, { v: 1 }, { etag: '"a"' }));
    const { fb } = make(f);
    await fb.request("/r", { conditional: true });
    fb.transport.clearConditionalCache();
    await fb.request("/r", { conditional: true });
    expect(f.calls[1]!.headers["If-None-Match"]).toBeUndefined();
  });
});
