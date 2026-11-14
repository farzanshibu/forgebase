import { describe, expect, it } from "vitest";
import { ConflictError, ForgeBase, RateLimitError } from "../src/index";
import { errBody, json, noSleep, scripted } from "./helpers";

const client = (...replies: Parameters<typeof scripted>) => {
  const s = scripted(...replies);
  return { fb: new ForgeBase("http://x", { apiKey: "k", fetch: s.fetch, retry: { ...noSleep, maxRetries: 1 } }), calls: s.calls };
};
const attempt = (id: number, extra: Record<string, unknown> = {}) => ({
  id, subscription: "orders-push", message_id: "m1", attempt: 1, test: false, kind: "webhook", endpoint: "https://e.test/hook", at: "2026-01-01T00:00:00Z",
  latency_ms: 12, outcome: "success", status: 200, request_headers: {}, response_headers: {}, response_truncated: false, ...extra,
});
const sub = { name: "orders-push", topic: "orders", ack_deadline_ms: 10000, backoff_base_ms: 1000, backoff_max_ms: 600000, ordering: false, labels: {}, created_by: "u", created_at: "t", updated_at: "t" };
const body = (c: { body?: string }) => JSON.parse(c.body ?? "null") as unknown;
const call = (c: { method: string; url: string }) => `${c.method} ${c.url.replace("http://x/api/v1/queues/subscriptions/orders-push", "")}`;

describe("queues.webhooks", () => {
  it("lists attempts with every filter and continues with next_before", async () => {
    const { fb, calls } = client(json(200, { items: [attempt(9)], next_before: 9 }));
    const page = await fb.queues.webhooks("orders-push").attempts({
      outcome: "failure", messageId: "m1", since: new Date("2026-01-01T00:00:00Z"), until: "2026-02-01T00:00:00Z", test: false, before: 20, limit: 5,
    });
    expect(page.next_before).toBe(9);
    const u = new URL(calls[0]!.url);
    expect(u.pathname).toBe("/api/v1/queues/subscriptions/orders-push/attempts");
    expect(Object.fromEntries(u.searchParams)).toEqual({ outcome: "failure", message_id: "m1", since: "2026-01-01T00:00:00.000Z", until: "2026-02-01T00:00:00Z", test: "false", before: "20", limit: "5" });
  });

  it("gets one attempt; the subscription name is escaped", async () => {
    const { fb, calls } = client(json(200, attempt(7)));
    expect((await fb.queues.webhooks("a b/c").attempt(7)).id).toBe(7);
    expect(calls[0]!.url).toBe("http://x/api/v1/queues/subscriptions/a%20b%2Fc/attempts/7");
  });

  it("sends a test event, with or without data, and never retries it", async () => {
    const { fb, calls } = client(json(200, attempt(1, { test: true })), json(200, attempt(2, { test: true })), json(429, errBody("rate_limited", "slow"), { "retry-after": "1" }));
    const w = fb.queues.webhooks("orders-push");
    expect((await w.test()).test).toBe(true);
    await w.test({ order: 1 });
    expect(call(calls[0]!)).toBe("POST /test");
    expect(body(calls[0]!)).toEqual({});
    expect(body(calls[1]!)).toEqual({ data: { order: 1 } });
    await expect(w.test()).rejects.toBeInstanceOf(RateLimitError);
    expect(calls).toHaveLength(3);
  });

  it("redrives by ids or everything", async () => {
    const result = { redriven: [1, 2], already_redriven: [], not_found: [], remaining: 0 };
    const { fb, calls } = client(json(200, result));
    const w = fb.queues.webhooks("orders-push");
    expect(await w.redrive({ ids: [1, 2] })).toEqual(result);
    await w.redrive({ all: true });
    await w.redrive({ all: true, since: new Date("2026-01-01T00:00:00Z"), filter: { kind: "rush" }, limit: 50 });
    expect(calls.map((c) => body(c))).toEqual([
      { ids: [1, 2] },
      { all: true },
      { all: true, since: "2026-01-01T00:00:00.000Z", filter: { kind: "rush" }, limit: 50 },
    ]);
    expect(call(calls[0]!)).toBe("POST /redrive");
  });

  it("retries now: all or chosen messages", async () => {
    const { fb, calls } = client(json(200, { count: 3 }));
    const w = fb.queues.webhooks("orders-push");
    expect((await w.retry()).count).toBe(3);
    await w.retry(["m1", "m2"]);
    expect(calls.map((c) => body(c))).toEqual([{}, { message_ids: ["m1", "m2"] }]);
    expect(call(calls[0]!)).toBe("POST /retry");
  });

  it("rotates the secret, expires the previous one", async () => {
    const { fb, calls } = client(json(200, { subscription: "orders-push", push_secret: "whsec_new", previous_expires_at: "2026-01-02T00:00:00Z" }), json(200, sub), json(200, sub));
    const w = fb.queues.webhooks("orders-push");
    expect((await w.rotateSecret({ graceMs: 3600_000 })).push_secret).toBe("whsec_new");
    await w.rotateSecret();
    await w.expirePreviousSecret();
    expect(call(calls[0]!)).toBe("POST /secret/rotate");
    expect(body(calls[0]!)).toEqual({ grace_ms: 3600_000 });
    expect(body(calls[1]!)).toEqual({});
    expect(call(calls[2]!)).toBe("POST /secret/expire-previous");
  });

  it("pauses with a reason and resumes", async () => {
    const paused = { ...sub, paused_at: "t", paused_reason: "deploy" };
    const { fb, calls } = client(json(200, paused), json(200, sub));
    const w = fb.queues.webhooks("orders-push");
    expect((await w.pause({ reason: "deploy" })).paused_reason).toBe("deploy");
    await w.resume();
    expect(call(calls[0]!)).toBe("POST /pause");
    expect(body(calls[0]!)).toEqual({ reason: "deploy" });
    expect(call(calls[1]!)).toBe("POST /resume");
  });

  it("a pull subscription answers 409 not_push_subscription", async () => {
    const { fb } = client(json(409, errBody("not_push_subscription", "pull")));
    await expect(fb.queues.webhooks("pull-sub").test()).rejects.toBeInstanceOf(ConflictError);
  });
});

describe("queues: catalog, patches and the new fields", () => {
  it("creates a topic with description and example, and updates it", async () => {
    const topic = { name: "orders", retention_ms: 1, max_message_bytes: 1, dedup_window_ms: 1, labels: {}, published_total: 0, published_bytes: 0, created_by: "u", created_at: "t", updated_at: "t", description: "Orders", example: { id: 1 } };
    const { fb, calls } = client(json(201, topic), json(200, topic));
    const t = await fb.queues.createTopic("orders", { description: "Orders", example: { id: 1 } });
    expect(t.description).toBe("Orders");
    expect(body(calls[0]!)).toEqual({ name: "orders", description: "Orders", example: { id: 1 } });
    await fb.queues.updateTopic("orders", { description: "", clearExample: true, retentionBytes: 0, clearSchema: true });
    expect(calls[1]).toMatchObject({ method: "PATCH", url: "http://x/api/v1/queues/topics/orders" });
    expect(body(calls[1]!)).toEqual({ description: "", clear_example: true, retention_bytes: 0, clear_schema: true });
  });

  it("creates a subscription with pause_after_failures and ttl_ms, and updates it", async () => {
    const { fb, calls } = client(json(201, { ...sub, pause_after_failures: 5, expires_at: "t" }), json(200, sub));
    const s = await fb.queues.createSubscription("orders-push", "orders", { push: { kind: "webhook", endpoint: "https://e.test/h" }, pauseAfterFailures: 5, ttlMs: 3600_000 });
    expect(s.pause_after_failures).toBe(5);
    expect(body(calls[0]!)).toEqual({ name: "orders-push", topic: "orders", push: { kind: "webhook", endpoint: "https://e.test/h" }, pause_after_failures: 5, ttl_ms: 3600_000 });
    await fb.queues.updateSubscription("orders-push", { pauseAfterFailures: 0, ttlMs: 0, clearPush: true, maxDeliveryAttempts: 0 });
    expect(calls[1]).toMatchObject({ method: "PATCH", url: "http://x/api/v1/queues/subscriptions/orders-push" });
    expect(body(calls[1]!)).toEqual({ pause_after_failures: 0, ttl_ms: 0, clear_push: true, max_delivery_attempts: 0 });
  });

  it("reads the event catalog", async () => {
    const cat = { topics: [{ topic: "orders", description: "Orders", example: { id: 1 }, example_source: "topic", event_types: [], data_schema: null }] };
    const { fb, calls } = client(json(200, cat));
    const c = await fb.queues.catalog();
    expect(c.topics[0]!.example_source).toBe("topic");
    expect(calls[0]).toMatchObject({ method: "GET", url: "http://x/api/v1/queues/catalog" });
  });
});
