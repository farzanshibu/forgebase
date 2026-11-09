import { describe, expect, it } from "vitest";
import {
  ConflictError, ForgeBase, NetworkError, NotFoundError, PermissionError, QuotaExceededError, RateLimitError, ServerError, ValidationError,
  parseRetryAfter, AuthenticationError,
} from "../src/index";
import { errBody, json, scripted } from "./helpers";

const make = (f: ReturnType<typeof scripted>, retry = {}) => {
  const sleeps: number[] = [];
  const fb = new ForgeBase("http://x.test/", {
    fetch: f.fetch,
    retry: { sleep: (ms) => (sleeps.push(ms), Promise.resolve()), random: () => 0.5, ...retry },
  });
  return { fb, sleeps };
};

describe("error mapping", () => {
  it.each([
    [400, "validation_failed", ValidationError],
    [401, "unauthenticated", AuthenticationError],
    [403, "forbidden", PermissionError],
    [404, "not_found", NotFoundError],
    [409, "conflict", ConflictError],
    [429, "rate_limited", RateLimitError],
    [429, "quota_exceeded", QuotaExceededError],
    [500, "internal", ServerError],
  ])("%i %s", async (status, code, cls) => {
    const { fb } = make(scripted(json(status, errBody(code, "boom", { fields: { title: { code: "required", message: "needed" } } }))), { maxRetries: 0 });
    const err = await fb.request("/api/v1/x", { anonymous: true }).catch((e: unknown) => e);
    expect(err).toBeInstanceOf(cls);
    expect(err).toMatchObject({ status, code, message: "boom", requestId: "req-1" });
    if (cls === ValidationError) expect((err as ValidationError).fields.title).toEqual({ code: "required", message: "needed" });
  });

  it("wraps connection failures in NetworkError (status 0)", async () => {
    const { fb } = make(scripted(new TypeError("fetch failed")), { maxRetries: 0 });
    await expect(fb.request("/x")).rejects.toMatchObject({ name: "NetworkError", status: 0, code: "network_error" });
    expect(await fb.request("/x").catch((e: unknown) => e)).toBeInstanceOf(NetworkError);
  });

  it("tolerates non-JSON error bodies", async () => {
    const { fb } = make(scripted(new Response("<html>bad gateway</html>", { status: 418 })), { maxRetries: 0 });
    await expect(fb.request("/x")).rejects.toMatchObject({ status: 418, code: "http_error" });
  });

  it("parses Retry-After as seconds and as an HTTP date", () => {
    expect(parseRetryAfter("3")).toBe(3000);
    expect(parseRetryAfter("1.5")).toBe(1500);
    expect(parseRetryAfter(new Date(10_000).toUTCString(), 4_000)).toBe(6000);
    expect(parseRetryAfter("soon")).toBeUndefined();
    expect(parseRetryAfter(null)).toBeUndefined();
  });
});

describe("retries", () => {
  it("retries 429 honouring Retry-After, then succeeds", async () => {
    const f = scripted(json(429, errBody("rate_limited"), { "retry-after": "2" }), json(429, errBody("rate_limited"), { "retry-after": "1" }), json(200, { ok: true }));
    const { fb, sleeps } = make(f);
    expect(await fb.request("/api/v1/x")).toEqual({ ok: true });
    expect(sleeps).toEqual([2000, 1000]);
    expect(f.calls).toHaveLength(3);
  });

  it("retries 503 with exponential backoff and jitter when there is no Retry-After", async () => {
    const f = scripted(json(503, errBody("unavailable")), json(503, errBody("unavailable")), json(503, errBody("unavailable")), json(200, {}));
    const { fb, sleeps } = make(f, { baseDelayMs: 100, maxDelayMs: 10_000, random: () => 0.5 });
    await fb.request("/x");
    expect(sleeps).toEqual([50, 100, 200]); // 0.5 * min(max, base * 2^(n-1))
  });

  it("caps the backoff at maxDelayMs", async () => {
    const f = scripted(json(503, errBody("u")), json(503, errBody("u")), json(503, errBody("u")), json(200, {}));
    const { fb, sleeps } = make(f, { baseDelayMs: 1000, maxDelayMs: 1500, random: () => 1 - Number.EPSILON });
    await fb.request("/x");
    expect(sleeps.every((s) => s < 1500)).toBe(true);
  });

  it("gives up after maxRetries with the last error", async () => {
    const f = scripted(json(503, errBody("unavailable")));
    const { fb } = make(f, { maxRetries: 2 });
    await expect(fb.request("/x")).rejects.toBeInstanceOf(ServerError);
    expect(f.calls).toHaveLength(3);
  });

  it("retries POST on 429/503 (the server rejected it before doing work) but not on 500", async () => {
    const a = scripted(json(503, errBody("u")), json(201, { id: "1" }));
    expect(await make(a).fb.request("/x", { method: "POST", body: { a: 1 } })).toEqual({ id: "1" });
    expect(a.calls).toHaveLength(2);
    expect(a.calls[1]!.body).toBe('{"a":1}');
    const b = scripted(json(500, errBody("internal")), json(201, {}));
    await expect(make(b).fb.request("/x", { method: "POST", body: {} })).rejects.toBeInstanceOf(ServerError);
    expect(b.calls).toHaveLength(1);
  });

  it("retries 502/504 and network errors for idempotent methods only", async () => {
    const g = scripted(json(502, errBody("bad_gateway")), new TypeError("reset"), json(200, { ok: 1 }));
    expect(await make(g).fb.request("/x")).toEqual({ ok: 1 });
    const p = scripted(json(502, errBody("bad_gateway")), json(200, {}));
    await expect(make(p).fb.request("/x", { method: "POST", body: {} })).rejects.toBeInstanceOf(ServerError);
    expect(p.calls).toHaveLength(1);
    const pn = scripted(new TypeError("reset"), json(200, {}));
    await expect(make(pn).fb.request("/x", { method: "POST", body: {} })).rejects.toBeInstanceOf(NetworkError);
  });

  it("does not retry a 4xx, nor an excessive Retry-After", async () => {
    const f = scripted(json(404, errBody("not_found")), json(200, {}));
    await expect(make(f).fb.request("/x")).rejects.toBeInstanceOf(NotFoundError);
    expect(f.calls).toHaveLength(1);
    const g = scripted(json(429, errBody("rate_limited"), { "retry-after": "3600" }), json(200, {}));
    const { fb, sleeps } = make(g);
    await expect(fb.request("/x")).rejects.toMatchObject({ retryAfter: 3600 });
    expect(sleeps).toEqual([]);
  });

  it("can be disabled per client or per call", async () => {
    const f = scripted(json(503, errBody("u")), json(200, {}));
    await expect(make(f, { maxRetries: 0 }).fb.request("/x")).rejects.toBeInstanceOf(ServerError);
    const g = scripted(json(503, errBody("u")), json(200, {}));
    await expect(make(g).fb.request("/x", { retry: false })).rejects.toBeInstanceOf(ServerError);
  });

  it("stops sleeping when the caller aborts", async () => {
    const ac = new AbortController();
    const f = scripted(json(503, errBody("u")));
    const fb = new ForgeBase("http://x.test", {
      fetch: f.fetch,
      retry: { sleep: (_ms, signal) => new Promise((_r, rej) => signal?.addEventListener("abort", () => rej(signal.reason))), random: () => 0 },
    });
    const p = fb.request("/x", { signal: ac.signal });
    setTimeout(() => ac.abort(new Error("stop")), 5);
    await expect(p).rejects.toThrow("stop");
  });
});
