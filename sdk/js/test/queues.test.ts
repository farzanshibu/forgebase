import { describe, expect, it } from "vitest";
import { ApiError, ForgeBase, type Delivery } from "../src/index";
import { errBody, json, noSleep, scripted } from "./helpers";

const client = (...replies: Parameters<typeof scripted>) => {
  const s = scripted(...replies);
  return { fb: new ForgeBase("http://x", { apiKey: "k", fetch: s.fetch, retry: { ...noSleep, statuses: [] } }), calls: s.calls };
};

const sse = (events: { event: string; data: unknown }[]): Response =>
  new Response(
    new ReadableStream<Uint8Array>({
      start(c) {
        const enc = new TextEncoder();
        for (const e of events) c.enqueue(enc.encode(`event: ${e.event}\ndata: ${JSON.stringify(e.data)}\n\n`));
        c.close();
      },
    }),
    { status: 200, headers: { "content-type": "text/event-stream" } },
  );

const received = (n: number) => ({
  ack_id: `a${n}`,
  delivery_attempt: 1,
  ack_deadline: "t",
  message: { id: `m${n}`, topic: "t", seq: n, data: { n }, attributes: { k: "v" }, publisher: "p", bytes: 1, published_at: "t" },
});

describe("queues", () => {
  it("publishes one message with attributes, ordering key and dedup id", async () => {
    const { fb, calls } = client(json(200, { results: [{ message_id: "m1", seq: 4, duplicate: false }] }));
    const r = await fb.queues.publish("orders", { id: 1 }, { attributes: { kind: "rush" }, orderingKey: "u1", dedupId: "d1" });
    expect(r).toEqual({ message_id: "m1", seq: 4, duplicate: false });
    expect(calls[0]!.url).toBe("http://x/api/v1/queues/topics/orders/publish");
    expect(calls[0]!.method).toBe("POST");
    expect(JSON.parse(calls[0]!.body!)).toEqual({ messages: [{ data: { id: 1 }, attributes: { kind: "rush" }, ordering_key: "u1", dedup_id: "d1" }] });
  });

  it("publishes a batch and surfaces a duplicate", async () => {
    const { fb } = client(json(200, { results: [{ message_id: "a", seq: 1, duplicate: false }, { message_id: "a", seq: 1, duplicate: true }] }));
    const r = await fb.queues.publishBatch("t", [{ data: 1, dedupId: "x" }, { data: 1, dedupId: "x" }]);
    expect(r.map((p) => p.duplicate)).toEqual([false, true]);
  });

  it("maps topic errors to ApiError with the code", async () => {
    const { fb } = client(json(409, errBody("topic_exists", "topic exists")));
    await expect(fb.queues.createTopic("t")).rejects.toMatchObject({ status: 409, code: "topic_exists" });
    await expect(fb.queues.createTopic("t")).rejects.toBeInstanceOf(ApiError);
  });

  it("creates a topic and a subscription with camelCase options mapped to the wire", async () => {
    const { fb, calls } = client(json(201, { name: "t" }), json(201, { name: "s" }));
    await fb.queues.createTopic("t", { retentionMs: 1000, dedupWindowMs: 5, labels: { a: "b" } });
    await fb.queues.createSubscription("s", "t", { ackDeadlineMs: 20_000, maxDeliveryAttempts: 3, deadLetterTopic: "dlq", ordering: true, start: "earliest" });
    expect(JSON.parse(calls[0]!.body!)).toEqual({ name: "t", retention_ms: 1000, dedup_window_ms: 5, labels: { a: "b" } });
    expect(JSON.parse(calls[1]!.body!)).toEqual({ name: "s", topic: "t", ack_deadline_ms: 20_000, max_delivery_attempts: 3, dead_letter_topic: "dlq", ordering: true, start: "earliest" });
  });

  it("pulls, acks and nacks by ack id", async () => {
    const { fb, calls } = client(json(200, { messages: [received(1), received(2)] }), json(200, { count: 1, unknown: [] }), json(200, { count: 1, unknown: ["a2"] }));
    const msgs = await fb.queues.pull<{ n: number }>("s", { maxMessages: 2, waitMs: 500 });
    expect(msgs.map((m) => m.message.data.n)).toEqual([1, 2]);
    expect(JSON.parse(calls[0]!.body!)).toEqual({ max_messages: 2, wait_ms: 500 });
    expect((await fb.queues.ack("s", ["a1"])).count).toBe(1);
    expect((await fb.queues.nack("s", ["a2"])).unknown).toEqual(["a2"]);
    expect(calls[1]!.url).toBe("http://x/api/v1/queues/subscriptions/s/ack");
    expect(JSON.parse(calls[2]!.body!)).toEqual({ ack_ids: ["a2"] });
  });

  it("seeks to a time or a snapshot", async () => {
    const { fb, calls } = client(json(200, { cursor_seq: 1, restored: 3 }));
    await fb.queues.seek("s", { time: new Date("2026-01-01T00:00:00Z") });
    await fb.queues.seek("s", { snapshot: "snap" });
    expect(JSON.parse(calls[0]!.body!)).toEqual({ time: "2026-01-01T00:00:00.000Z" });
    expect(JSON.parse(calls[1]!.body!)).toEqual({ snapshot: "snap" });
  });

  it("pages the message log", async () => {
    const { fb, calls } = client(json(200, { items: [], next_after: 9 }));
    const r = await fb.queues.messages("t", { after: 5, limit: 2 });
    expect(r.nextAfter).toBe(9);
    expect(calls[0]!.url).toContain("after=5");
  });

  it("encodes names in paths", async () => {
    const { fb, calls } = client(json(200, { name: "a/b" }));
    await fb.queues.getTopic("a/b");
    expect(calls[0]!.url).toBe("http://x/api/v1/queues/topics/a%2Fb");
  });
});

describe("queues.subscribe (streaming pull)", () => {
  it("acks messages whose handler resolves and nacks the ones that throw", async () => {
    const { fb, calls } = client(
      sse([{ event: "message", data: received(1) }, { event: "ping", data: {} }, { event: "message", data: received(2) }]),
      json(200, { count: 1, unknown: [] }),
    );
    const errors: unknown[] = [];
    const seen: number[] = [];
    const stream = fb.queues.subscribe<{ n: number }>(
      "s",
      (d) => {
        seen.push(d.data.n);
        expect(d.attempt).toBe(1);
        expect(d.attributes).toEqual({ k: "v" });
        if (d.data.n === 2) throw new Error("boom");
      },
      { reconnect: false, onError: (e) => errors.push(e) },
    );
    await stream.done;
    expect(seen).toEqual([1, 2]);
    const urls = calls.map((c) => `${c.method} ${c.url.replace("http://x/api/v1/queues/subscriptions/", "")}`);
    expect(urls[0]).toBe("GET s/events");
    expect(urls.slice(1).sort()).toEqual(["POST s/ack", "POST s/nack"]);
    expect(JSON.parse(calls.find((c) => c.url.endsWith("/nack"))!.body!)).toEqual({ ack_ids: ["a2"] });
    expect(errors).toHaveLength(1);
    expect(calls[0]!.headers.Accept).toBe("text/event-stream");
  });

  it("leaves settling to the handler with autoAck off", async () => {
    const { fb, calls } = client(sse([{ event: "message", data: received(1) }]), json(200, { count: 1, unknown: [] }));
    let d: Delivery | undefined;
    const stream = fb.queues.subscribe("s", async (x) => { d = x; }, { autoAck: false, reconnect: false });
    await stream.done;
    expect(calls).toHaveLength(1);
    await d!.ack();
    await d!.ack(); // idempotent
    expect(calls.filter((c) => c.url.endsWith("/ack"))).toHaveLength(1);
  });

  it("passes flow-control limits as query parameters", async () => {
    const { fb, calls } = client(sse([]));
    await fb.queues.subscribe("s", () => undefined, { maxMessages: 5, maxBytes: 1024, reconnect: false }).done;
    expect(calls[0]!.url).toContain("max_messages=5");
    expect(calls[0]!.url).toContain("max_bytes=1024");
  });

  it("rejects done when the stream carries an error event", async () => {
    const { fb } = client(sse([{ event: "error", data: { code: "push_subscription", message: "push subscription" } }]));
    await expect(fb.queues.subscribe("s", () => undefined).done).rejects.toMatchObject({ code: "push_subscription" });
  });

  it("rejects done on a 403 without retrying", async () => {
    const { fb, calls } = client(json(403, errBody("forbidden", "no")));
    await expect(fb.queues.subscribe("s", () => undefined).done).rejects.toMatchObject({ status: 403 });
    expect(calls).toHaveLength(1);
  });

  it("reconnects after the stream ends, until unsubscribed", async () => {
    const s = scripted(() => sse([]));
    const fb = new ForgeBase("http://x", { apiKey: "k", fetch: s.fetch, retry: { sleep: () => new Promise((r) => setTimeout(r, 1)), random: () => 0.5, statuses: [] } });
    const calls = s.calls;
    const stream = fb.queues.subscribe("s", () => undefined);
    for (let i = 0; i < 200 && calls.length < 3; i++) await new Promise((r) => setTimeout(r, 5));
    stream.unsubscribe();
    await stream.done;
    expect(calls.length).toBeGreaterThanOrEqual(3);
  });
});
