import { describe, expect, it } from "vitest";
import { ConflictError, ForgeBase, NotFoundError, PermissionError, ServerError, ValidationError } from "../src/index";
import { VectorApi } from "../src/vector";
import { errBody, json, scripted } from "./helpers";

const field = { name: "embedding", dimension: 3, metric: "cosine", quantization: "f32", connectivity: 16, expansion_add: 128, expansion_search: 128, created: "t" };
const coll = { name: "docs", description: "", fields: [field], exact_threshold: 5000, items: 2, created: "t", updated: "t" };
const index = { field: "embedding", state: "ready", serving: "ann", canonical_vectors: 2, indexed_vectors: 2, memory_bytes: 1, serialized_bytes: 1, capacity: 8, engine: "usearch", engine_version: "2", quantization: "f32", connectivity: 16, expansion_search: 128, dirty: false, searches: 0, ann_searches: 0, exact_searches: 0, mean_search_ms: 0 };

const setup = (...r: Parameters<typeof scripted>) => {
  const f = scripted(...r);
  return { f, v: new VectorApi(new ForgeBase("http://x", { fetch: f.fetch }).transport) };
};
const body = (c: { body?: string }) => JSON.parse(c.body!);

describe("vector collections", () => {
  it("list unwraps items; stats hits the tenant route", async () => {
    const { f, v } = setup(json(200, { items: [coll] }), json(200, { collections: 1, indexes: 1, building: 0, indexed_vectors: 2, memory_bytes: 1 }));
    expect(await v.list()).toEqual([coll]);
    expect(await v.stats()).toMatchObject({ collections: 1 });
    expect(f.calls.map((c) => c.url)).toEqual(["http://x/api/v1/vector/collections", "http://x/api/v1/vector/stats"]);
  });

  it("create maps camelCase options to the wire body", async () => {
    const { f, v } = setup(json(201, coll));
    await v.create({
      name: "docs",
      description: "d",
      exactThreshold: 100,
      sourceCollection: "posts",
      fields: [{ name: "embedding", dimension: 3, metric: "cosine", quantization: "i8", connectivity: 8, expansionAdd: 64, expansionSearch: 32 }],
    });
    expect(f.calls[0]).toMatchObject({ method: "POST", url: "http://x/api/v1/vector/collections" });
    expect(body(f.calls[0]!)).toEqual({
      name: "docs",
      description: "d",
      exact_threshold: 100,
      source_collection: "posts",
      fields: [{ name: "embedding", dimension: 3, metric: "cosine", quantization: "i8", connectivity: 8, expansion_add: 64, expansion_search: 32 }],
    });
  });

  it("create with only a name omits everything else", async () => {
    const { f, v } = setup(json(201, coll));
    await v.create({ name: "docs" });
    expect(body(f.calls[0]!)).toEqual({ name: "docs" });
  });

  it("get and drop address the collection by an encoded name", async () => {
    const { f, v } = setup(json(200, coll), json(204, null));
    const c = v.collection("my docs");
    expect(await c.get()).toEqual(coll);
    expect(await c.drop()).toBeUndefined();
    expect(f.calls.map((x) => `${x.method} ${x.url}`)).toEqual(["GET http://x/api/v1/vector/collections/my%20docs", "DELETE http://x/api/v1/vector/collections/my%20docs"]);
  });

  it("a taken name is a ConflictError, an unknown one a NotFoundError", async () => {
    const { v } = setup(json(409, errBody("collection_exists")), json(404, errBody("collection_not_found")));
    await expect(v.create({ name: "docs" })).rejects.toBeInstanceOf(ConflictError);
    await expect(v.collection("nope").get()).rejects.toBeInstanceOf(NotFoundError);
  });
});

describe("vector fields", () => {
  it("list, add and drop", async () => {
    const { f, v } = setup(json(200, { items: [field] }), json(201, field), json(204, null));
    const fields = v.collection("docs").fields;
    expect(await fields.list()).toEqual([field]);
    await fields.add({ name: "title_vec", dimension: 8, metric: "l2", expansionSearch: 256 });
    await fields.drop("title vec");
    expect(f.calls.map((c) => `${c.method} ${c.url}`)).toEqual([
      "GET http://x/api/v1/vector/collections/docs/fields",
      "POST http://x/api/v1/vector/collections/docs/fields",
      "DELETE http://x/api/v1/vector/collections/docs/fields/title%20vec",
    ]);
    expect(body(f.calls[1]!)).toEqual({ name: "title_vec", dimension: 8, metric: "l2", expansion_search: 256 });
  });
});

describe("vector items", () => {
  it("upsert wraps a single item and keeps a batch as is", async () => {
    const { f, v } = setup(json(200, { ids: ["a"], created: 1, updated: 0 }), json(200, { ids: ["b", "c"], created: 0, updated: 2 }));
    const c = v.collection("docs");
    expect(await c.upsert({ id: "a", vectors: { embedding: [1, 0, 0] }, metadata: { category: "news" }, text: "hello" })).toEqual({ ids: ["a"], created: 1, updated: 0 });
    await c.upsert([{ id: "b" }, { id: "c", vectors: { embedding: [0, 1, 0] } }]);
    expect(f.calls[0]).toMatchObject({ method: "POST", url: "http://x/api/v1/vector/collections/docs/upsert" });
    expect(body(f.calls[0]!)).toEqual({ items: [{ id: "a", vectors: { embedding: [1, 0, 0] }, metadata: { category: "news" }, text: "hello" }] });
    expect(body(f.calls[1]!).items).toHaveLength(2);
  });

  it("a wrong dimension is a ValidationError", async () => {
    const { v } = setup(json(400, errBody("invalid_dimension", "expected 3 components")));
    await expect(v.collection("docs").upsert({ vectors: { embedding: [1] } })).rejects.toBeInstanceOf(ValidationError);
  });

  it("getItem, deleteItem and deleteItems (POST /delete)", async () => {
    const item = { id: "a/b", vectors: { embedding: [1, 0, 0] }, metadata: { k: 1 }, text: null, version: 1, created: "t", updated: "t" };
    const { f, v } = setup(json(200, item), json(204, null), json(200, { deleted: 2 }));
    const c = v.collection("docs");
    expect(await c.getItem("a/b")).toEqual(item);
    await c.deleteItem("a/b");
    expect(await c.deleteItems(["a", "b", "ghost"])).toEqual({ deleted: 2 });
    expect(f.calls.map((x) => `${x.method} ${x.url}`)).toEqual([
      "GET http://x/api/v1/vector/collections/docs/items/a%2Fb",
      "DELETE http://x/api/v1/vector/collections/docs/items/a%2Fb",
      "POST http://x/api/v1/vector/collections/docs/delete",
    ]);
    expect(body(f.calls[2]!)).toEqual({ ids: ["a", "b", "ghost"] });
  });
});

describe("vector search", () => {
  it("search maps every option", async () => {
    const res = { hits: [{ id: "a", distance: 0, score: 1, metadata: { category: "news" } }], strategy: "filtered_ann", candidates: 4, took_ms: 0.4 };
    const { f, v } = setup(json(200, res));
    const out = await v.collection("docs").search({
      field: "embedding",
      vector: [1, 0, 0],
      k: 5,
      filter: "category = {:c} && year >= 2020",
      params: { c: "news" },
      filterMode: "pre",
      exact: false,
      includeVectors: true,
      includeMetadata: true,
      includeText: true,
    });
    expect(out.hits[0]!.metadata).toEqual({ category: "news" });
    expect(out.strategy).toBe("filtered_ann");
    expect(f.calls[0]).toMatchObject({ method: "POST", url: "http://x/api/v1/vector/collections/docs/search" });
    expect(body(f.calls[0]!)).toEqual({
      field: "embedding",
      vector: [1, 0, 0],
      k: 5,
      filter: "category = {:c} && year >= 2020",
      params: { c: "news" },
      filter_mode: "pre",
      exact: false,
      include_vectors: true,
      include_metadata: true,
      include_text: true,
    });
  });

  it("search with only a vector sends only the vector", async () => {
    const { f, v } = setup(json(200, { hits: [], strategy: "ann", took_ms: 0 }));
    await v.collection("docs").search({ vector: [0.5, 0.5, 0] });
    expect(body(f.calls[0]!)).toEqual({ vector: [0.5, 0.5, 0] });
  });

  it("a bad filter is a ValidationError; a server fault a ServerError", async () => {
    const { v } = setup(json(400, errBody("invalid_filter")), json(500, errBody("internal")));
    const c = v.collection("docs");
    await expect(c.search({ vector: [1], filter: "((" })).rejects.toBeInstanceOf(ValidationError);
    await expect(c.search({ vector: [1] })).rejects.toBeInstanceOf(ServerError);
  });

  it("hybrid maps weights, rrf and keyword options", async () => {
    const res = { hits: [{ id: "a", score: 0.03, vector_rank: 1, text_rank: 2 }], vector_strategy: "ann", vector_candidates: 50, text_candidates: 3, took_ms: 1 };
    const { f, v } = setup(json(200, res), json(200, res));
    const c = v.collection("docs");
    expect(await c.hybrid({ vector: [1, 0, 0], text: "rust db", textOperator: "and", k: 3, candidates: 80, rrfK: 30, weights: { vector: 2, text: 0.5 }, filter: "year > 2000", filterMode: "post" })).toEqual(res);
    await c.hybrid({ text: "keyword only" });
    expect(f.calls[0]).toMatchObject({ method: "POST", url: "http://x/api/v1/vector/collections/docs/hybrid" });
    expect(body(f.calls[0]!)).toEqual({
      vector: [1, 0, 0],
      text: "rust db",
      text_operator: "and",
      k: 3,
      candidates: 80,
      rrf_k: 30,
      weights: { vector: 2, text: 0.5 },
      filter: "year > 2000",
      filter_mode: "post",
    });
    expect(body(f.calls[1]!)).toEqual({ text: "keyword only" });
  });
});

describe("vector maintenance", () => {
  it("stats", async () => {
    const stats = { collection: "docs", items: 2, items_with_text: 1, change_seq: 4, indexes: [index] };
    const { f, v } = setup(json(200, stats));
    expect(await v.collection("docs").stats()).toEqual(stats);
    expect(f.calls[0]!.url).toBe("http://x/api/v1/vector/collections/docs/stats");
  });

  it("rebuild unwraps the index list (202) and can target one field", async () => {
    const building = { ...index, state: "building", progress: { done: 1, total: 2 } };
    const { f, v } = setup(json(202, { indexes: [building] }), json(202, { indexes: [index] }));
    const c = v.collection("docs");
    expect(await c.rebuild()).toEqual([building]);
    await c.rebuild("embedding");
    expect(f.calls[0]).toMatchObject({ method: "POST", url: "http://x/api/v1/vector/collections/docs/rebuild" });
    expect(body(f.calls[0]!)).toEqual({});
    expect(body(f.calls[1]!)).toEqual({ field: "embedding" });
  });

  it("recall maps samples and k; admin actions need permission", async () => {
    const report = { field: "embedding", k: 10, samples: 100, recall: 0.99, ann_ms: 1, exact_ms: 9, measured: "t" };
    const { f, v } = setup(json(200, report), json(200, report), json(403, errBody("forbidden")));
    const c = v.collection("docs");
    expect(await c.recall({ field: "embedding", samples: 50, k: 5 })).toEqual(report);
    await c.recall();
    expect(body(f.calls[0]!)).toEqual({ field: "embedding", samples: 50, k: 5 });
    expect(body(f.calls[1]!)).toEqual({});
    expect(f.calls[0]!.url).toBe("http://x/api/v1/vector/collections/docs/recall");
    await expect(c.recall()).rejects.toBeInstanceOf(PermissionError);
  });
});
