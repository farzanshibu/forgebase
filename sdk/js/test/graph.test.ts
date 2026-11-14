import { describe, expect, it } from "vitest";
import { ConflictError, ForgeBase, NotFoundError, PermissionError, ValidationError } from "../src/index";
import { GraphApi } from "../src/graph";
import { errBody, json, scripted } from "./helpers";

const node = (id: string, extra: object = {}) => ({ id, label: "User", properties: { name: id }, created_at: "t", updated_at: "t", ...extra });
const edge = (id: string, extra: object = {}) => ({ id, type: "FOLLOWS", source: "a", target: "b", directed: true, properties: {}, created_at: "t", updated_at: "t", ...extra });

const setup = (...r: Parameters<typeof scripted>) => {
  const f = scripted(...r);
  return { f, g: new GraphApi(new ForgeBase("http://x", { fetch: f.fetch }).transport) };
};
const body = (c: { body?: string }) => JSON.parse(c.body!);

describe("graph.nodes", () => {
  it("create sends only the given fields and returns the node", async () => {
    const { f, g } = setup(json(201, node("u1")), json(201, node("g1")));
    expect(await g.nodes.create({ id: "u1", label: "User", properties: { name: "u1" } })).toMatchObject({ id: "u1" });
    await g.nodes.create();
    expect(f.calls[0]).toMatchObject({ method: "POST", url: "http://x/api/v1/graph/nodes" });
    expect(body(f.calls[0]!)).toEqual({ id: "u1", label: "User", properties: { name: "u1" } });
    expect(body(f.calls[1]!)).toEqual({});
  });

  it("list maps filters to the query and nextAfter", async () => {
    const { f, g } = setup(json(200, { items: [node("a")], next_after: "a" }), json(200, { items: [] }));
    expect(await g.nodes.list({ label: "User", after: "x", limit: 50 })).toEqual({ items: [node("a")], nextAfter: "a" });
    expect((await g.nodes.list()).nextAfter).toBeNull();
    expect(f.calls[0]!.url).toBe("http://x/api/v1/graph/nodes?label=User&after=x&limit=50");
    expect(f.calls[1]!.url).toBe("http://x/api/v1/graph/nodes");
  });

  it("pages follows nextAfter until the last page", async () => {
    const { f, g } = setup(json(200, { items: [node("a")], next_after: "a" }), json(200, { items: [node("b")], next_after: "b" }), json(200, { items: [node("c")] }));
    const ids: string[] = [];
    for await (const p of g.nodes.pages({ limit: 1 })) ids.push(...p.items.map((n) => n.id));
    expect(ids).toEqual(["a", "b", "c"]);
    expect(f.calls.map((c) => c.url)).toEqual([
      "http://x/api/v1/graph/nodes?limit=1",
      "http://x/api/v1/graph/nodes?after=a&limit=1",
      "http://x/api/v1/graph/nodes?after=b&limit=1",
    ]);
  });

  it("get, replace (PUT), patch (PATCH) and delete address the node by an encoded id", async () => {
    const { f, g } = setup(json(200, node("a:1")), json(200, node("a:1")), json(200, node("a:1")), json(200, { id: "a:1", deleted_edges: 3 }));
    await g.nodes.get("a:1");
    await g.nodes.replace("a:1", { label: "Admin", properties: { k: 1 } });
    await g.nodes.patch("a:1", { properties: { gone: null } });
    expect(await g.nodes.delete("a:1")).toEqual({ id: "a:1", deleted_edges: 3 });
    expect(f.calls.map((c) => `${c.method} ${c.url}`)).toEqual([
      "GET http://x/api/v1/graph/nodes/a%3A1",
      "PUT http://x/api/v1/graph/nodes/a%3A1",
      "PATCH http://x/api/v1/graph/nodes/a%3A1",
      "DELETE http://x/api/v1/graph/nodes/a%3A1",
    ]);
    expect(body(f.calls[1]!)).toEqual({ label: "Admin", properties: { k: 1 } });
    expect(body(f.calls[2]!)).toEqual({ properties: { gone: null } });
  });

  it("neighbors joins edge types and passes direction and paging", async () => {
    const hop = { direction: "out", edge: edge("e1"), node: node("b") };
    const { f, g } = setup(json(200, { items: [hop], next_after: "100" }));
    expect(await g.nodes.neighbors("a", { direction: "both", types: ["FOLLOWS", "LIKES"], limit: 100, offset: 0 })).toEqual({ items: [hop], nextAfter: "100" });
    expect(f.calls[0]!.url).toBe("http://x/api/v1/graph/nodes/a/neighbors?direction=both&type=FOLLOWS%2CLIKES&limit=100&offset=0");
  });

  it("maps errors: duplicate id, missing node, rejected input", async () => {
    const { g } = setup(json(409, errBody("node_exists")), json(404, errBody("node_not_found")), json(400, errBody("invalid_input")));
    await expect(g.nodes.create({ id: "a" })).rejects.toBeInstanceOf(ConflictError);
    await expect(g.nodes.get("zz")).rejects.toBeInstanceOf(NotFoundError);
    await expect(g.nodes.create({ id: "bad id" })).rejects.toBeInstanceOf(ValidationError);
  });
});

describe("graph.edges", () => {
  it("create maps the body", async () => {
    const { f, g } = setup(json(201, edge("e1", { directed: false })));
    await g.edges.create({ type: "KNOWS", source: "a", target: "b", directed: false, properties: { since: 2020 } });
    expect(f.calls[0]).toMatchObject({ method: "POST", url: "http://x/api/v1/graph/edges" });
    expect(body(f.calls[0]!)).toEqual({ type: "KNOWS", source: "a", target: "b", directed: false, properties: { since: 2020 } });
  });

  it("list filters and paginates", async () => {
    const { f, g } = setup(json(200, { items: [edge("e1")], next_after: "e1" }), json(200, { items: [edge("e1")], next_after: "e1" }), json(200, { items: [edge("e2")] }));
    expect(await g.edges.list({ type: "FOLLOWS", source: "a", target: "b", after: "e0", limit: 10 })).toMatchObject({ nextAfter: "e1" });
    expect(f.calls[0]!.url).toBe("http://x/api/v1/graph/edges?type=FOLLOWS&source=a&target=b&after=e0&limit=10");
    const seen: string[] = [];
    for await (const p of g.edges.pages()) seen.push(...p.items.map((e) => e.id));
    expect(seen).toEqual(["e1", "e2"]);
  });

  it("get, patch and delete", async () => {
    const { f, g } = setup(json(200, edge("e/1")), json(200, edge("e/1")), json(204, null));
    await g.edges.get("e/1");
    await g.edges.patch("e/1", { weight: 2, old: null });
    expect(await g.edges.delete("e/1")).toBeUndefined();
    expect(f.calls.map((c) => `${c.method} ${c.url}`)).toEqual([
      "GET http://x/api/v1/graph/edges/e%2F1",
      "PATCH http://x/api/v1/graph/edges/e%2F1",
      "DELETE http://x/api/v1/graph/edges/e%2F1",
    ]);
    expect(body(f.calls[1]!)).toEqual({ properties: { weight: 2, old: null } });
  });

  it("a missing endpoint is a NotFoundError", async () => {
    const { g } = setup(json(404, errBody("node_not_found")));
    await expect(g.edges.create({ type: "T", source: "a", target: "zz" })).rejects.toBeInstanceOf(NotFoundError);
  });
});

describe("graph queries", () => {
  it("traverse maps options to the wire body", async () => {
    const result = { start: "a", nodes: [{ depth: 1, node: node("b") }], edges: [edge("e1")], truncated: false };
    const { f, g } = setup(json(200, result));
    expect(
      await g.traverse({ start: "a", direction: "both", edgeTypes: ["FOLLOWS"], maxDepth: 2, limit: 10, filter: { label: "User", properties: { country: "IN" } }, includeStart: true, includeEdges: false }),
    ).toEqual(result);
    expect(f.calls[0]).toMatchObject({ method: "POST", url: "http://x/api/v1/graph/traverse" });
    expect(body(f.calls[0]!)).toEqual({
      start: "a",
      direction: "both",
      edge_types: ["FOLLOWS"],
      max_depth: 2,
      limit: 10,
      filter: { label: "User", properties: { country: "IN" } },
      include_start: true,
      include_edges: false,
    });
  });

  it("traverse with only a start sends only the start", async () => {
    const { f, g } = setup(json(200, { start: "a", nodes: [], edges: [], truncated: false }));
    await g.traverse({ start: "a" });
    expect(body(f.calls[0]!)).toEqual({ start: "a" });
  });

  it("shortestPath maps from/to and reports found: false", async () => {
    const { f, g } = setup(json(200, { found: false, nodes: [], edges: [] }));
    expect(await g.shortestPath({ from: "a", to: "z", direction: "out", edgeTypes: ["T"], maxDepth: 4 })).toMatchObject({ found: false });
    expect(f.calls[0]!.url).toBe("http://x/api/v1/graph/shortest-path");
    expect(body(f.calls[0]!)).toEqual({ from: "a", to: "z", direction: "out", edge_types: ["T"], max_depth: 4 });
  });

  it("stats", async () => {
    const stats = { nodes: 3, edges: 2, edge_types: [{ type: "FOLLOWS", count: 2 }], cypher: { state: "ready", graph: "g", nodes: 3, edges: 2, done: 5 } };
    const { f, g } = setup(json(200, stats));
    expect(await g.stats()).toEqual(stats);
    expect(f.calls[0]!.url).toBe("http://x/api/v1/graph/stats");
  });
});

describe("graph.cypher", () => {
  it("sends the query, params, limit and timeout", async () => {
    const result = { columns: ["n"], rows: [[{ id: "a", label: "User", properties: {} }]], truncated: false, engine: "portable" };
    const { f, g } = setup(json(200, result));
    expect(await g.cypher("MATCH (n:User {id: $id}) RETURN n", { params: { id: "a" }, limit: 5, timeoutMs: 2000 })).toEqual(result);
    expect(f.calls[0]).toMatchObject({ method: "POST", url: "http://x/api/v1/graph/cypher" });
    expect(body(f.calls[0]!)).toEqual({ query: "MATCH (n:User {id: $id}) RETURN n", params: { id: "a" }, limit: 5, timeout_ms: 2000 });
  });

  it("the first query of a tenant is a ConflictError carrying the job id", async () => {
    const { g } = setup(json(409, { error: { code: "graph_materialising", message: "building", request_id: "r", job_id: "job_1" } }));
    const err = await g.cypher("MATCH (n) RETURN n").catch((e: unknown) => e);
    expect(err).toBeInstanceOf(ConflictError);
    expect(err).toMatchObject({ code: "graph_materialising", body: { error: { job_id: "job_1" } } });
  });

  it("a caller without engine:graph_cypher is a PermissionError", async () => {
    const { g } = setup(json(403, errBody("forbidden")));
    await expect(g.cypher("MATCH (n) RETURN n")).rejects.toBeInstanceOf(PermissionError);
  });

  it("dropCypher", async () => {
    const { f, g } = setup(json(200, { dropped: true }));
    expect(await g.dropCypher()).toEqual({ dropped: true });
    expect(f.calls[0]).toMatchObject({ method: "POST", url: "http://x/api/v1/graph/cypher/drop" });
  });
});

describe("graph.cypher engine", () => {
  it("reports which engine answered", async () => {
    const { g } = setup(json(200, { columns: ["c"], rows: [[1]], truncated: false, engine: "age" }), json(200, { columns: ["c"], rows: [[1]], truncated: false, engine: "portable" }));
    expect((await g.cypher("MATCH (n) RETURN count(n) AS c")).engine).toBe("age");
    expect((await g.cypher("MATCH (n) RETURN count(n) AS c")).engine).toBe("portable");
  });
});
