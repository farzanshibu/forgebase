import { describe, expect, it } from "vitest";
import { ConflictError, NotFoundError, Transport } from "../src/index";
import { IndexesApi } from "../src/indexes";
import { errBody, json, noSleep, scripted } from "./helpers";

const setup = (...r: Parameters<typeof scripted>) => {
  const f = scripted(...r);
  return { f, ix: new IndexesApi(new Transport({ baseUrl: "http://x", fetch: f.fetch, retry: { ...noSleep, statuses: [] } })) };
};
const body = (c: { body?: string }) => JSON.parse(c.body!) as unknown;

const info = { name: "ix_posts_title", table: "posts", collection: "posts", kind: "btree", origin: "manual", columns: ["title"], unique: false, partial: false, stats: { rows_estimate: 10 } };

describe("indexes", () => {
  it("lists, optionally for one collection", async () => {
    const { f, ix } = setup(json(200, { items: [info] }), json(200, { items: [] }));
    expect(await ix.list()).toEqual([info]);
    expect(await ix.list({ collection: "blog posts" })).toEqual([]);
    expect(f.calls.map((c) => c.url)).toEqual(["http://x/api/v1/indexes", "http://x/api/v1/indexes?collection=blog+posts"]);
  });

  it("creates an index: bare string keys become ascending paths", async () => {
    const { f, ix } = setup(json(201, info));
    const r = await ix.create({ collection: "posts", keys: ["title", { path: "created", desc: true }, { path: "a" }], unique: true, partial: "published = true", kind: "btree", name: "ix1" });
    expect(r).toEqual(info);
    expect(f.calls[0]).toMatchObject({ method: "POST", url: "http://x/api/v1/indexes" });
    expect(body(f.calls[0]!)).toEqual({
      name: "ix1",
      collection: "posts",
      keys: [{ path: "title" }, { path: "created", desc: true }, { path: "a" }],
      unique: true,
      partial: "published = true",
      kind: "btree",
    });
  });

  it("omits unset fields on create", async () => {
    const { f, ix } = setup(json(201, info));
    await ix.create({ collection: "posts", keys: ["body"], kind: "fulltext" });
    expect(body(f.calls[0]!)).toEqual({ collection: "posts", keys: [{ path: "body" }], kind: "fulltext" });
  });

  it("maps a duplicate to ConflictError", async () => {
    const { ix } = setup(json(409, errBody("index_exists", "name taken")));
    await expect(ix.create({ collection: "posts", keys: ["a"] })).rejects.toBeInstanceOf(ConflictError);
  });

  it("gets, rebuilds, reads stats and drops by name", async () => {
    const { f, ix } = setup(json(200, info), json(200, info), json(200, { rows_estimate: 10, size_bytes: 4096 }), json(204, null));
    expect(await ix.get("a/b")).toEqual(info);
    expect(await ix.rebuild("a/b")).toEqual(info);
    expect(await ix.stats("a/b")).toEqual({ rows_estimate: 10, size_bytes: 4096 });
    await ix.drop("a/b");
    expect(f.calls.map((c) => `${c.method} ${c.url.replace("http://x/api/v1/indexes", "")}`)).toEqual([
      "GET /a%2Fb",
      "POST /a%2Fb/rebuild",
      "GET /a%2Fb/stats",
      "DELETE /a%2Fb",
    ]);
  });

  it("maps a missing index to NotFoundError", async () => {
    const { ix } = setup(json(404, errBody("index_not_found", "no such index")));
    await expect(ix.get("nope")).rejects.toBeInstanceOf(NotFoundError);
  });

  it("recommends and analyzes", async () => {
    const rec = { definition: { collection: "posts", keys: [{ path: "title" }] }, reason: "slow", occurrences: 4, max_duration_ms: 120 };
    const { f, ix } = setup(json(200, { items: [rec] }), json(204, null), json(204, null));
    expect(await ix.recommendations()).toEqual([rec]);
    await ix.analyze();
    await ix.analyze({ collection: "posts" });
    expect(f.calls[0]!.url).toBe("http://x/api/v1/indexes/recommendations");
    expect(f.calls[1]).toMatchObject({ method: "POST", url: "http://x/api/v1/indexes/analyze" });
    expect(body(f.calls[1]!)).toEqual({});
    expect(body(f.calls[2]!)).toEqual({ collection: "posts" });
  });

  it("reads, clears and tunes the slow-query log", async () => {
    const log = { threshold_ms: 100, items: [{ at: "t", duration_ms: 250, rows: 3, param_count: 1, sql: "SELECT 1" }] };
    const { f, ix } = setup(json(200, log), json(204, null), json(200, { threshold_ms: 0 }));
    expect(await ix.slowQueries()).toEqual(log);
    await ix.clearSlowQueries();
    expect(await ix.setSlowQueryThreshold(0)).toBe(0);
    expect(f.calls.map((c) => `${c.method} ${c.url.replace("http://x/api/v1/", "")}`)).toEqual(["GET slow-queries", "DELETE slow-queries", "PUT slow-queries/threshold"]);
    expect(body(f.calls[2]!)).toEqual({ threshold_ms: 0 });
  });
});
