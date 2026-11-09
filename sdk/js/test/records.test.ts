import { describe, expect, it } from "vitest";
import { ForgeBase } from "../src/index";
import { json, scripted } from "./helpers";

const page = (items: unknown[], extra: object = {}) => json(200, { items, per_page: 2, ...extra });
const mk = (...r: Parameters<typeof scripted>) => {
  const f = scripted(...r);
  return { f, posts: new ForgeBase("http://x", { fetch: f.fetch }).collection("posts") };
};

describe("records", () => {
  it("builds the list query", async () => {
    const { f, posts } = mk(page([], { page: 2, total_items: 0, total_pages: 0 }));
    await posts.list({ filter: 'a = "b"', sort: ["-created", "name"], expand: ["author"], fields: ["a", "b"], page: 2, perPage: 5, search: "q", skipTotal: true });
    const u = new URL(f.calls[0]!.url);
    expect(u.pathname).toBe("/api/v1/collections/posts/records");
    expect(Object.fromEntries(u.searchParams)).toEqual({ filter: 'a = "b"', sort: "-created,name", expand: "author", fields: "a,b", page: "2", per_page: "5", search: "q", skip_total: "true" });
  });

  it("uses the typed JSON query endpoint when params are given", async () => {
    const { f, posts } = mk(page([{ id: "1" }]));
    const r = await posts.list({ filter: "n > {:min}", params: { min: 3 }, expand: ["a"], perPage: 10 });
    expect(f.calls[0]).toMatchObject({ method: "POST" });
    expect(f.calls[0]!.url).toBe("http://x/api/v1/collections/posts/records/query");
    expect(JSON.parse(f.calls[0]!.body!)).toMatchObject({ filter: "n > {:min}", params: { min: 3 }, expand: ["a"], per_page: 10 });
    expect(r.items).toEqual([{ id: "1" }]);
  });

  it("maps pagination fields to camelCase", async () => {
    const { posts } = mk(page([], { page: 1, total_items: 9, total_pages: 5, next_cursor: "c" }));
    expect(await posts.list()).toEqual({ items: [], page: 1, perPage: 2, totalItems: 9, totalPages: 5, nextCursor: "c" });
  });

  it("iterates by cursor and stops when next_cursor is absent", async () => {
    const { f, posts } = mk(page([{ id: 1 }, { id: 2 }], { next_cursor: "c1" }), page([{ id: 3 }], { next_cursor: "c2" }), page([{ id: 4 }]));
    expect((await posts.all()).map((r) => r.id)).toEqual([1, 2, 3, 4]);
    expect(f.calls.map((c) => new URL(c.url).searchParams.get("cursor"))).toEqual([null, "c1", "c2"]);
    expect(new URL(f.calls[1]!.url).searchParams.has("page")).toBe(false);
  });

  it("iterates by page number when the server reports totals without cursors", async () => {
    const { f, posts } = mk(page([{ id: 1 }], { page: 1, total_pages: 2 }), page([{ id: 2 }], { page: 2, total_pages: 2 }));
    expect((await posts.all({ perPage: 1 })).map((r) => r.id)).toEqual([1, 2]);
    expect(f.calls.map((c) => new URL(c.url).searchParams.get("page"))).toEqual(["1", "2"]);
  });

  it("lets the consumer stop early without fetching more pages", async () => {
    const { f, posts } = mk(page([{ id: 1 }], { next_cursor: "c" }), page([{ id: 2 }]));
    for await (const r of posts.iterate()) {
      expect(r.id).toBe(1);
      break;
    }
    expect(f.calls).toHaveLength(1);
  });

  it("writes with If-Match when a version is given and encodes ids", async () => {
    const { f, posts } = mk(json(200, { id: "a/b" }), json(200, { id: "a/b" }), json(204, null));
    await posts.update("a/b", { x: 1 }, { version: 7 });
    await posts.replace("a/b", { x: 2 });
    await posts.delete("a/b");
    expect(f.calls[0]).toMatchObject({ method: "PATCH", url: "http://x/api/v1/collections/posts/records/a%2Fb" });
    expect(f.calls[0]!.headers["If-Match"]).toBe('"7"');
    expect(f.calls[1]!.headers["If-Match"]).toBeUndefined();
    expect(f.calls[2]!.method).toBe("DELETE");
  });

  it("exports as text and imports raw", async () => {
    const f = scripted(new Response('{"id":1}\n', { status: 200 }), json(200, { imported: 1, format: "ndjson" }));
    const posts = new ForgeBase("http://x", { fetch: f.fetch }).collection("posts");
    expect(await posts.export("ndjson")).toBe('{"id":1}\n');
    expect(await posts.import("ndjson", '{"a":1}')).toEqual({ imported: 1, format: "ndjson" });
    expect(f.calls[1]).toMatchObject({ method: "POST", body: '{"a":1}' });
    expect(new URL(f.calls[1]!.url).searchParams.get("format")).toBe("ndjson");
  });
});
