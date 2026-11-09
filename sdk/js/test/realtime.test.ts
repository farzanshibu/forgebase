import { describe, expect, it } from "vitest";
import { ForgeBase, type RealtimeEvent, type RecordEvent, type WebSocketLike } from "../src/index";
import { json, scripted } from "./helpers";

class FakeSocket implements WebSocketLike {
  static all: FakeSocket[] = [];
  readyState = 0;
  sent: Record<string, unknown>[] = [];
  onopen: ((ev: unknown) => void) | null = null;
  onmessage: ((ev: { data: unknown }) => void) | null = null;
  onclose: ((ev: { code: number; reason: string }) => void) | null = null;
  onerror: ((ev: unknown) => void) | null = null;
  /** Server behaviour: reply to subscribe with `subscribed` carrying this head cursor. */
  static head = 10;
  static refuse: string | undefined;
  constructor(readonly url: string) {
    FakeSocket.all.push(this);
    queueMicrotask(() => {
      this.readyState = 1;
      this.onopen?.({});
      this.push({ type: "welcome", connection_id: "c", heartbeat_ms: 60_000 });
    });
  }
  push(m: object) {
    this.onmessage?.({ data: JSON.stringify(m) });
  }
  send(data: string) {
    const m = JSON.parse(data) as Record<string, unknown>;
    this.sent.push(m);
    if (m.type === "subscribe") {
      if (FakeSocket.refuse) this.push({ type: "error", ref: m.ref, id: m.id, code: "forbidden", message: FakeSocket.refuse });
      else this.push({ type: "subscribed", ref: m.ref, id: m.id, cursor: m.cursor ?? FakeSocket.head });
    } else if (m.type === "publish") this.push({ type: "ack", ref: m.ref });
    else if (m.type === "presence.join") this.push({ type: "presence.state", ref: m.ref, channel: m.channel, members: [{ connection_id: "c", actor: "a", joined_at: "t" }] });
    else if (m.type === "unsubscribe") this.push({ type: "unsubscribed", ref: m.ref, id: m.id });
  }
  close(code = 1000, reason = "") {
    this.readyState = 3;
    queueMicrotask(() => this.onclose?.({ code, reason }));
  }
}

const fresh = () => {
  FakeSocket.all = [];
  FakeSocket.head = 10;
  FakeSocket.refuse = undefined;
  return new ForgeBase("http://x", {
    accessToken: "tok",
    retry: { sleep: () => Promise.resolve(), random: () => 0 },
    realtime: { WebSocket: FakeSocket, reconnect: { baseDelayMs: 1, maxDelayMs: 2 } },
  });
};
const ev = (n: number, id = "s1"): RecordEvent => ({ type: "record.created", id, cursor: n, collection: "c", record_id: `r${n}`, timestamp: "t", data: { n } });
const until = async (fn: () => unknown) => {
  for (let i = 0; i < 400 && !fn(); i++) await new Promise((r) => setTimeout(r, 5));
  if (!fn()) throw new Error("timeout");
};

describe("realtime over WebSocket", () => {
  it("connects with the token in the query string, subscribes and tracks the cursor", async () => {
    const fb = fresh();
    const got: RealtimeEvent[] = [];
    const sub = await fb.realtime.subscribe({ id: "s1", collection: "c", filter: "a = 1" }, (e) => got.push(e));
    const ws = FakeSocket.all[0]!;
    expect(ws.url).toBe("ws://x/api/v1/realtime/ws?access_token=tok");
    expect(ws.sent[0]).toMatchObject({ type: "subscribe", id: "s1", collection: "c", filter: "a = 1" });
    expect(sub.cursor).toBe(10);
    ws.push(ev(11));
    ws.push(ev(12));
    ws.push(ev(11)); // a duplicate never moves the cursor back
    expect(got).toHaveLength(3);
    expect(sub.cursor).toBe(12);
    fb.realtime.close();
  });

  it("resumes from the last cursor after the socket drops", async () => {
    const fb = fresh();
    const got: RecordEvent[] = [];
    const states: string[] = [];
    fb.realtime.on("reconnecting", (a) => states.push(`retry${a}`));
    fb.realtime.on("open", () => states.push("open"));
    await fb.realtime.subscribe({ id: "s1", collection: "c" }, (e) => got.push(e as RecordEvent));
    FakeSocket.all[0]!.push(ev(11));
    FakeSocket.all[0]!.close(1006, "gone");
    await until(() => FakeSocket.all.length === 2 && FakeSocket.all[1]!.sent.length > 0);
    expect(FakeSocket.all[1]!.sent[0]).toMatchObject({ type: "subscribe", id: "s1", cursor: 11 });
    FakeSocket.all[1]!.push(ev(12));
    expect(got.map((e) => e.cursor)).toEqual([11, 12]);
    expect(states).toContain("retry1");
    fb.realtime.close();
  });

  it("delivers reset events and moves the cursor to the new head", async () => {
    const fb = fresh();
    const got: RealtimeEvent[] = [];
    const sub = await fb.realtime.subscribe({ id: "s1", collection: "c", cursor: 3 }, (e) => got.push(e));
    FakeSocket.all[0]!.push({ type: "reset", id: "s1", reason: "cursor_expired", cursor: 500 });
    expect(got[0]).toMatchObject({ type: "reset", reason: "cursor_expired" });
    expect(sub.cursor).toBe(500);
    fb.realtime.close();
  });

  it("rejects a subscription the server refuses", async () => {
    const fb = fresh();
    FakeSocket.refuse = "no access";
    await expect(fb.realtime.subscribe({ collection: "secret" }, () => undefined)).rejects.toMatchObject({ code: "forbidden", message: "no access" });
    fb.realtime.close();
  });

  it("publishes, joins presence and unsubscribes over the socket", async () => {
    const fb = fresh();
    await fb.realtime.subscribe({ id: "ch", channel: "room" }, () => undefined);
    await fb.realtime.publish("room", { hi: 1 });
    expect((await fb.realtime.joinPresence("room", { name: "x" }))[0]!.actor).toBe("a");
    const sub = fb.realtime;
    await sub.unsubscribe("ch");
    const types = FakeSocket.all[0]!.sent.map((m) => m.type);
    expect(types).toEqual(["subscribe", "publish", "presence.join", "unsubscribe"]);
    fb.realtime.close();
  });

  it("forwards a rotated access token to the server", async () => {
    const fb = fresh();
    await fb.auth.setSession({ accessToken: "a1", expiresAt: Date.now() + 1e6, sessionId: "s", sessionExpiresAt: "x", subject: {}, cookie: false });
    // Static tokens take precedence in the transport, but the session listener still fires on rotation.
    await fb.realtime.subscribe({ id: "s1", collection: "c" }, () => undefined);
    await fb.auth.setSession({ accessToken: "a2", expiresAt: Date.now() + 1e6, sessionId: "s", sessionExpiresAt: "x", subject: {}, cookie: false }, "token_refreshed");
    expect(FakeSocket.all[0]!.sent.at(-1)).toEqual({ type: "auth", token: "a2" });
    fb.realtime.close();
  });

  it("refreshes the token and reconnects after close code 4001", async () => {
    const fb = fresh();
    await fb.realtime.subscribe({ id: "s1", collection: "c" }, () => undefined);
    FakeSocket.all[0]!.close(4001, "revoked");
    await until(() => FakeSocket.all.length === 2);
    expect(fb.realtime.state === "open" || fb.realtime.state === "reconnecting" || fb.realtime.state === "connecting").toBe(true);
    fb.realtime.close();
  });

  it("stays quiet after close()", async () => {
    const fb = fresh();
    await fb.realtime.subscribe({ id: "s1", collection: "c" }, () => undefined);
    fb.realtime.close();
    await new Promise((r) => setTimeout(r, 30));
    expect(FakeSocket.all).toHaveLength(1);
    expect(fb.realtime.state).toBe("closed");
  });
});

const sseResponse = (chunks: string[], hold = false): Response => {
  const enc = new TextEncoder();
  return new Response(
    new ReadableStream({
      start(c) {
        for (const x of chunks) c.enqueue(enc.encode(x));
        if (!hold) c.close();
      },
    }),
    { status: 200, headers: { "content-type": "text/event-stream" } },
  );
};

describe("realtime over SSE", () => {
  it("parses events split across chunks, ignores heartbeats and resumes with the cursor", async () => {
    const first = sseResponse([
      'event: subscribed\ndata: {"id":"s1","cursor":5}\n\n',
      ": ping\n\n",
      'id: 6\nevent: record.created\ndata: {"id":"s1","cursor":6,"colle',
      'ction":"c","record_id":"r6","timestamp":"t","data":{"n":6}}\n\n',
    ]);
    const second = sseResponse(['event: subscribed\ndata: {"id":"s1","cursor":6}\n\nid: 7\nevent: record.updated\ndata: {"id":"s1","cursor":7,"collection":"c","record_id":"r6","timestamp":"t","data":{"n":7}}\n\n'], true);
    const f = scripted(first, second);
    const fb = new ForgeBase("http://x", { fetch: f.fetch, accessToken: "tok", retry: { sleep: () => Promise.resolve(), random: () => 0 }, realtime: { transport: "sse", reconnect: { baseDelayMs: 1, maxDelayMs: 2 } } });
    const got: RecordEvent[] = [];
    const sub = await fb.realtime.subscribe({ id: "s1", collection: "c" }, (e) => got.push(e as RecordEvent));
    await until(() => got.length === 2);
    expect(got.map((e) => e.cursor)).toEqual([6, 7]);
    expect(sub.cursor).toBe(7);
    const u0 = new URL(f.calls[0]!.url);
    expect(u0.pathname).toBe("/api/v1/realtime/sse");
    expect(f.calls[0]!.headers.Authorization).toBe("Bearer tok");
    expect(JSON.parse(u0.searchParams.get("subscriptions")!)).toEqual([{ id: "s1", collection: "c" }]);
    // The stream ended after cursor 6, so the reconnect asked for everything after it.
    expect(JSON.parse(new URL(f.calls[1]!.url).searchParams.get("subscriptions")!)).toEqual([{ id: "s1", collection: "c", cursor: 6 }]);
    fb.realtime.close();
  });

  it("rejects when the server answers 403", async () => {
    const f = scripted(json(403, { error: { code: "forbidden", message: "nope", request_id: "r" } }));
    const fb = new ForgeBase("http://x", { fetch: f.fetch, accessToken: "tok", realtime: { transport: "sse" } });
    await expect(fb.realtime.subscribe({ collection: "c" }, () => undefined)).rejects.toMatchObject({ status: 403, code: "forbidden" });
    fb.realtime.close();
  });

  it("publishes through REST and refuses presence joins", async () => {
    const f = scripted(json(200, {}));
    const fb = new ForgeBase("http://x", { fetch: f.fetch, accessToken: "tok", realtime: { transport: "sse" } });
    await fb.realtime.publish("room", { a: 1 });
    expect(f.calls[0]).toMatchObject({ method: "POST", url: "http://x/api/v1/realtime/channels/room/messages" });
    expect(JSON.parse(f.calls[0]!.body!)).toEqual({ data: { a: 1 } });
    await expect(fb.realtime.joinPresence("room")).rejects.toMatchObject({ code: "unsupported_transport" });
  });
});
