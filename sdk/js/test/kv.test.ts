import { describe, expect, it } from "vitest";
import { ApiError, NotFoundError, Transport } from "../src/index";
import { KvApi, kvBytes, kvText } from "../src/kv";
import { errBody, json, noSleep, scripted } from "./helpers";

const setup = (...r: Parameters<typeof scripted>) => {
  const f = scripted(...r);
  const t = new Transport({ baseUrl: "http://x", fetch: f.fetch, retry: { ...noSleep, statuses: [] } });
  const kv = new KvApi(t, () => new Promise((r) => setTimeout(r, 1)));
  return { f, kv, cache: kv.ns("cache") };
};
const body = (c: { body?: string }) => JSON.parse(c.body!) as unknown;
const sse = (events: { event: string; data: unknown }[]): Response =>
  new Response(
    new ReadableStream<Uint8Array>({
      start(c) {
        const enc = new TextEncoder();
        for (const e of events) c.enqueue(enc.encode(`event: ${e.event}\ndata: ${typeof e.data === "string" ? e.data : JSON.stringify(e.data)}\n\n`));
        c.close();
      },
    }),
    { status: 200, headers: { "content-type": "text/event-stream" } },
  );

describe("kv admin", () => {
  it("info, namespaces and flush", async () => {
    const { f, kv } = setup(json(200, { scope: "engine", keys: 3 }), json(200, { namespaces: [{ name: "cache", keys: 3 }] }), json(200, { deleted: 3 }));
    expect(await kv.info()).toMatchObject({ keys: 3 });
    expect(await kv.namespaces()).toEqual([{ name: "cache", keys: 3 }]);
    expect(await kv.flush("a/b")).toBe(3);
    expect(f.calls.map((c) => `${c.method} ${c.url}`)).toEqual([
      "GET http://x/api/v1/kv/_info",
      "GET http://x/api/v1/kv/_namespaces",
      "DELETE http://x/api/v1/kv/a%2Fb",
    ]);
  });

  it("ns() binds the namespace name", () => {
    const { kv } = setup();
    expect(kv.ns("cache").name).toBe("cache");
  });
});

describe("kv keys and strings", () => {
  it("scans with every filter and walks all pages with keys()", async () => {
    const { f, cache } = setup(
      json(200, { cursor: "k2", keys: ["a", "b"] }),
      json(200, { cursor: "0", keys: ["c"] }),
      json(200, { cursor: "0", keys: [] }),
    );
    const out: string[] = [];
    for await (const k of cache.keys({ match: "user:*", count: 2, type: "hash" })) out.push(k);
    expect(out).toEqual(["a", "b", "c"]);
    expect(f.calls[0]!.url).toBe("http://x/api/v1/kv/cache/keys?match=user%3A*&count=2&type=hash");
    expect(f.calls[1]!.url).toBe("http://x/api/v1/kv/cache/keys?cursor=k2&match=user%3A*&count=2&type=hash");
    expect(await cache.scan({ cursor: "0" })).toEqual({ cursor: "0", keys: [] });
    expect(f.calls[2]!.url).toBe("http://x/api/v1/kv/cache/keys?cursor=0");
  });

  it("gets a key with a limit and encodes the key", async () => {
    const reply = { key: "a b/c", type: "hash", ttl_ms: null, size: 1, truncated: false, value: { f: "v", raw: { base64: "AAE=" } } };
    const { f, cache } = setup(json(200, reply));
    const k = await cache.get("a b/c", { limit: 5 });
    expect(k).toEqual(reply);
    if (k.type === "hash") expect(kvText(k.value.f!)).toBe("v");
    expect(f.calls[0]!.url).toBe("http://x/api/v1/kv/cache/keys/a%20b%2Fc?limit=5");
  });

  it("maps a missing key to NotFoundError", async () => {
    const { cache } = setup(json(404, errBody("kv_key_not_found", "no such key")));
    await expect(cache.get("x")).rejects.toBeInstanceOf(NotFoundError);
    await expect(cache.get("x")).rejects.toMatchObject({ code: "kv_key_not_found", status: 404 });
  });

  it("deletes a key", async () => {
    const { f, cache } = setup(json(200, { deleted: true }), json(200, { deleted: false }));
    expect(await cache.delete("k")).toBe(true);
    expect(await cache.delete("k")).toBe(false);
    expect(f.calls[0]).toMatchObject({ method: "DELETE", url: "http://x/api/v1/kv/cache/keys/k" });
  });

  it("sets and clears a TTL", async () => {
    const { f, cache } = setup(json(200, { applied: true }), json(200, { persisted: true }), json(404, errBody("kv_key_not_found")));
    await cache.expire("k", 1500);
    expect(await cache.persist("k")).toBe(true);
    await expect(cache.persist("gone")).rejects.toBeInstanceOf(NotFoundError);
    expect(f.calls[0]).toMatchObject({ method: "PUT", url: "http://x/api/v1/kv/cache/keys/k/ttl" });
    expect(body(f.calls[0]!)).toEqual({ ttl_ms: 1500 });
    expect(f.calls[1]).toMatchObject({ method: "DELETE", url: "http://x/api/v1/kv/cache/keys/k/ttl" });
  });

  it("sets a string with options and bytes as base64", async () => {
    const { f, cache } = setup(json(200, { stored: true }), json(200, { stored: false }), json(200, { stored: true }));
    expect(await cache.set("k", "v", { ttlMs: 1000, nx: true, keepTtl: false })).toBe(true);
    expect(await cache.set("k", "v", { xx: true })).toBe(false);
    await cache.set("bin", new Uint8Array([0, 1, 255]));
    expect(f.calls[0]).toMatchObject({ method: "PUT", url: "http://x/api/v1/kv/cache/strings/k" });
    expect(body(f.calls[0]!)).toEqual({ value: "v", ttl_ms: 1000, nx: true, keepttl: false });
    expect(body(f.calls[1]!)).toEqual({ value: "v", xx: true });
    expect(body(f.calls[2]!)).toEqual({ value: { base64: "AAH/" } });
  });

  it("reads text through mget and decodes bytes", async () => {
    const { f, cache } = setup(json(200, { values: ["hello"] }), json(200, { values: [null] }), json(200, { values: ["a", { base64: "AAE=" }, null] }));
    expect(await cache.text("k")).toBe("hello");
    expect(await cache.text("none")).toBeNull();
    expect(await cache.mget(["a", "b", "c"])).toEqual(["a", { base64: "AAE=" }, null]);
    expect(body(f.calls[2]!)).toEqual({ keys: ["a", "b", "c"] });
    expect(Array.from(kvBytes({ base64: "AAH/" }))).toEqual([0, 1, 255]);
    expect(Array.from(kvBytes("é"))).toEqual([0xc3, 0xa9]);
    expect(kvText({ base64: "w6k=" })).toBe("é");
  });

  it("increments", async () => {
    const { f, cache } = setup(json(200, { value: 1 }), json(200, { value: -4 }));
    expect(await cache.incr("hits")).toBe(1);
    expect(await cache.incr("hits", -5)).toBe(-4);
    expect(f.calls[0]).toMatchObject({ method: "POST", url: "http://x/api/v1/kv/cache/strings/hits/incr" });
    expect(body(f.calls[0]!)).toEqual({});
    expect(body(f.calls[1]!)).toEqual({ by: -5 });
  });

  it("msets from an object or from pairs", async () => {
    const { f, cache } = setup(json(200, { stored: 2 }), json(200, { stored: 1 }));
    expect(await cache.mset({ a: "1", b: new Uint8Array([1]) })).toBe(2);
    expect(await cache.mset([["c", "3"]])).toBe(1);
    expect(f.calls[0]!.url).toBe("http://x/api/v1/kv/cache/mset");
    expect(body(f.calls[0]!)).toEqual({ entries: [{ key: "a", value: "1" }, { key: "b", value: { base64: "AQ==" } }] });
    expect(body(f.calls[1]!)).toEqual({ entries: [{ key: "c", value: "3" }] });
  });
});

describe("kv collections", () => {
  it("hashes", async () => {
    const { f, cache } = setup(json(200, { added: 2 }), json(200, { removed: 1 }), json(200, { value: 7 }), json(200, { value: 1 }));
    expect(await cache.hashSet("h", { a: "1", b: { base64: "Ag==" } })).toBe(2);
    expect(await cache.hashDelete("h", ["a"])).toBe(1);
    expect(await cache.hashIncr("h", "n", 7)).toBe(7);
    expect(await cache.hashIncr("h", "n")).toBe(1);
    expect(f.calls.map((c) => c.url.replace("http://x/api/v1/kv/cache/", ""))).toEqual(["hashes/h/set", "hashes/h/delete", "hashes/h/incr", "hashes/h/incr"]);
    expect(body(f.calls[0]!)).toEqual({ fields: { a: "1", b: { base64: "Ag==" } } });
    expect(body(f.calls[1]!)).toEqual({ fields: ["a"] });
    expect(body(f.calls[2]!)).toEqual({ field: "n", by: 7 });
    expect(body(f.calls[3]!)).toEqual({ field: "n" });
  });

  it("lists", async () => {
    const { f, cache } = setup(json(200, { length: 3 }), json(200, { values: ["a"] }), json(200, { values: [] }), json(200, { removed: 2 }), json(200, { ok: true }));
    expect(await cache.listPush("l", "left", ["a", "b", new Uint8Array([9])])).toBe(3);
    expect(await cache.listPop("l", "right", 1)).toEqual(["a"]);
    expect(await cache.listPop("l", "left")).toEqual([]);
    expect(await cache.listRemove("l", "a", -1)).toBe(2);
    await cache.listSet("l", -1, "z");
    expect(body(f.calls[0]!)).toEqual({ side: "left", values: ["a", "b", { base64: "CQ==" }] });
    expect(body(f.calls[1]!)).toEqual({ side: "right", count: 1 });
    expect(body(f.calls[2]!)).toEqual({ side: "left" });
    expect(body(f.calls[3]!)).toEqual({ value: "a", count: -1 });
    expect(body(f.calls[4]!)).toEqual({ index: -1, value: "z" });
    expect(f.calls.map((c) => c.url.split("/").slice(-1)[0])).toEqual(["push", "pop", "pop", "remove", "set"]);
  });

  it("surfaces an out-of-range LSET as a validation error", async () => {
    const { cache } = setup(json(400, errBody("kv_out_of_range", "index out of range")));
    await expect(cache.listSet("l", 99, "z")).rejects.toMatchObject({ status: 400, code: "kv_out_of_range", name: "ValidationError" });
  });

  it("sets and sorted sets", async () => {
    const { f, cache } = setup(json(200, { added: 2 }), json(200, { removed: 1 }), json(200, { added: 1 }), json(200, { removed: 1 }));
    expect(await cache.setAdd("s", ["a", "b"])).toBe(2);
    expect(await cache.setRemove("s", ["a"])).toBe(1);
    expect(await cache.zsetAdd("z", [{ member: "a", score: 1.5 }])).toBe(1);
    expect(await cache.zsetRemove("z", ["a"])).toBe(1);
    expect(f.calls.map((c) => c.url.replace("http://x/api/v1/kv/cache/", ""))).toEqual(["sets/s/add", "sets/s/remove", "zsets/z/add", "zsets/z/remove"]);
    expect(body(f.calls[0]!)).toEqual({ members: ["a", "b"] });
    expect(body(f.calls[2]!)).toEqual({ members: [{ member: "a", score: 1.5 }] });
  });

  it("streams", async () => {
    const entries = [{ id: "1-0", fields: { k: "v" } }];
    const { f, cache } = setup(json(201, { id: "1-0" }), json(200, { removed: 1 }), json(200, { entries }), json(200, { entries }), json(200, { entries: [] }));
    expect(await cache.streamAdd("x", { k: "v" }, { id: "1-0", maxLen: 100 })).toBe("1-0");
    expect(await cache.streamDelete("x", ["1-0"])).toBe(1);
    expect(await cache.streamRange("x", { start: "1-0", end: "+", count: 10, rev: true })).toEqual(entries);
    expect(await cache.streamRead("x", { after: "$", count: 5 })).toEqual(entries);
    expect(await cache.streamRange("x")).toEqual([]);
    expect(body(f.calls[0]!)).toEqual({ id: "1-0", fields: { k: "v" }, maxlen: 100 });
    expect(body(f.calls[1]!)).toEqual({ ids: ["1-0"] });
    expect(f.calls[2]!.url).toBe("http://x/api/v1/kv/cache/streams/x/range?start=1-0&end=%2B&count=10&rev=true");
    expect(f.calls[3]!.url).toBe("http://x/api/v1/kv/cache/streams/x/read?after=%24&count=5");
    expect(f.calls[4]!.url).toBe("http://x/api/v1/kv/cache/streams/x/range");
  });
});

describe("kv commands", () => {
  it("exec sends numbers in decimal and bytes as base64", async () => {
    const { f, cache } = setup(json(200, { result: ["a", 1, null, { base64: "AA==" }] }));
    expect(await cache.exec(["ZRANGE", "board", 0, -1, "WITHSCORES", 10n, new Uint8Array([0])])).toEqual(["a", 1, null, { base64: "AA==" }]);
    expect(f.calls[0]).toMatchObject({ method: "POST", url: "http://x/api/v1/kv/cache/exec" });
    expect(body(f.calls[0]!)).toEqual({ command: ["ZRANGE", "board", "0", "-1", "WITHSCORES", "10", { base64: "AA==" }] });
  });

  it("batch returns per-command results and errors", async () => {
    const results = [{ result: "OK" }, { error: { code: "kv_wrong_type", message: "wrong type" } }];
    const { f, cache } = setup(json(200, { results }));
    expect(await cache.batch([["SET", "a", "1"], ["LPUSH", "a", 1]])).toEqual(results);
    expect(body(f.calls[0]!)).toEqual({ commands: [["SET", "a", "1"], ["LPUSH", "a", "1"]] });
  });

  it("maps a bad command to ValidationError", async () => {
    const { cache } = setup(json(400, errBody("kv_syntax", "empty command")));
    await expect(cache.exec([])).rejects.toMatchObject({ status: 400, code: "kv_syntax" });
  });
});

describe("kv pub/sub", () => {
  it("publishes and lists channels", async () => {
    const { f, cache } = setup(json(200, { receivers: 2 }), json(200, { channels: [{ name: "news", subscribers: 2 }] }));
    expect(await cache.publish("news/1", "hi")).toBe(2);
    expect(await cache.channels()).toEqual([{ name: "news", subscribers: 2 }]);
    expect(f.calls[0]).toMatchObject({ method: "POST", url: "http://x/api/v1/kv/cache/publish/news%2F1" });
    expect(body(f.calls[0]!)).toEqual({ message: "hi" });
    expect(f.calls[1]!.url).toBe("http://x/api/v1/kv/cache/channels");
  });

  it("subscribes over SSE and reports lag and handler errors", async () => {
    const { f, cache } = setup(
      sse([
        { event: "message", data: { channel: "news", message: "one" } },
        { event: "lagged", data: "4" },
        { event: "message", data: { channel: "news", message: { base64: "AA==" } } },
        { event: "message", data: { channel: "news", message: "boom" } },
      ]),
    );
    const got: unknown[] = [];
    const errors: unknown[] = [];
    let lagged = 0;
    const sub = cache.subscribe(
      "news",
      (m) => {
        if (m.message === "boom") throw new Error("handler failed");
        got.push(m.message);
      },
      { reconnect: false, onLagged: (n) => (lagged = n), onError: (e) => errors.push(e) },
    );
    await sub.done;
    expect(got).toEqual(["one", { base64: "AA==" }]);
    expect(lagged).toBe(4);
    expect(errors).toHaveLength(1);
    expect(f.calls[0]!.url).toBe("http://x/api/v1/kv/cache/subscribe/news");
    expect(f.calls[0]!.headers.Accept).toBe("text/event-stream");
  });

  it("rejects done on a refusal without retrying", async () => {
    const { f, cache } = setup(json(403, errBody("forbidden", "no")));
    await expect(cache.subscribe("news", () => undefined).done).rejects.toBeInstanceOf(ApiError);
    expect(f.calls).toHaveLength(1);
  });

  it("reconnects after the stream ends, until unsubscribed", async () => {
    const { f, cache } = setup(() => sse([]));
    const sub = cache.subscribe("news", () => undefined);
    for (let i = 0; i < 200 && f.calls.length < 3; i++) await new Promise((r) => setTimeout(r, 5));
    sub.unsubscribe();
    await sub.done;
    expect(f.calls.length).toBeGreaterThanOrEqual(3);
  });
});
