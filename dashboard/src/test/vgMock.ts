/** In-memory fakes of the vector (ADR-0012) and graph (ADR-0006) REST APIs for component tests. Every request and response is checked against the real OpenAPI document. */
import type { GraphEdge, GraphNode, VectorCollection, VectorItem } from "../api";
import { envelope, type MockApi, type MockReply, type MockRequest } from "./mockApi";
import { hasResponse, matchRoute, responseSchemaName, validateBody } from "./contract";

const NOW = "2026-11-09T10:00:00Z";

type Handler = (r: MockRequest) => MockReply;

/** Register `method template` on `mock`, checking it exists in openapi.json and that the answer fits the documented schema. */
function route(mock: MockApi, method: string, template: string, h: Handler) {
  mock.on(method, template, (req) => {
    const spec = matchRoute(method, req.path);
    if (!spec) mock.problems.push(`not in openapi.json: ${method} ${req.path}`);
    const out = h(req);
    if (spec && out.status < 400) {
      if (!hasResponse(spec.op, out.status)) mock.problems.push(`${method} ${template}: status ${out.status} not documented`);
      const name = responseSchemaName(spec.op, out.status);
      if (name && out.body !== undefined) for (const p of validateBody(name, out.body)) mock.problems.push(`${method} ${template} -> ${name}: ${p}`);
    }
    return out;
  });
}

const dist = (a: number[], b: number[]) => Math.sqrt(a.reduce((s, x, i) => s + (x - (b[i] ?? 0)) ** 2, 0));

export interface VectorState { collections: VectorCollection[]; items: Record<string, VectorItem[]>; rebuilds: number }

export function installVector(mock: MockApi): VectorState {
  const st: VectorState = {
    collections: [{
      name: "docs", description: "Docs", exact_threshold: 1000, items: 0, created: NOW, updated: NOW,
      fields: [{ name: "embedding", dimension: 3, metric: "l2", quantization: "f32", connectivity: 16, expansion_add: 128, expansion_search: 64, created: NOW }],
    }],
    items: { docs: [] }, rebuilds: 0,
  };
  const mk = (id: string, v: number[], metadata: Record<string, unknown>, text: string | null): VectorItem => ({ id, vectors: { embedding: v }, metadata, text, version: 1, created: NOW, updated: NOW });
  st.items.docs = [mk("a", [1, 0, 0], { tag: "x" }, "red apple"), mk("b", [0, 1, 0], { tag: "y" }, "green pear"), mk("c", [0.9, 0.1, 0], { tag: "x" }, "red cherry")];
  const sync = () => st.collections.forEach((c) => (c.items = (st.items[c.name] ?? []).length));
  const nf = () => envelope(404, "not_found", "no such collection");
  const col = (n: string) => st.collections.find((c) => c.name === n);
  const status = (c: VectorCollection) => c.fields.map((f) => ({
    field: f.name, state: "ready", serving: "ann", canonical_vectors: (st.items[c.name] ?? []).length, indexed_vectors: (st.items[c.name] ?? []).length,
    memory_bytes: 2048, serialized_bytes: 1024, capacity: 1024, engine: "usearch", engine_version: "2", quantization: f.quantization, connectivity: f.connectivity,
    expansion_search: f.expansion_search, dirty: false, searches: 4, ann_searches: 3, exact_searches: 1, mean_search_ms: 0.5,
    ...(st.rebuilds ? { last_rebuild: { reason: "manual", detail: "", vectors: 3, duration_ms: 7, finished: NOW } } : {}),
  }));
  const V = "/api/v1/vector";
  route(mock, "GET", `${V}/stats`, () => ({ status: 200, body: { collections: st.collections.length, indexes: st.collections.length, building: 0, indexed_vectors: 3, memory_bytes: 2048 } }));
  route(mock, "GET", `${V}/collections`, () => { sync(); return { status: 200, body: { items: st.collections } }; });
  route(mock, "POST", `${V}/collections`, ({ body }) => {
    const b = body as { name: string; description?: string; fields?: { name: string; dimension: number; metric: "cosine" | "l2" | "dot"; quantization?: "f32" }[] };
    if (col(b.name)) return envelope(409, "conflict", "collection exists");
    const c: VectorCollection = {
      name: b.name, description: b.description ?? "", exact_threshold: 1000, items: 0, created: NOW, updated: NOW,
      fields: (b.fields ?? []).map((f) => ({ ...f, quantization: f.quantization ?? "f32", connectivity: 16, expansion_add: 128, expansion_search: 64, created: NOW })),
    };
    st.collections.push(c);
    st.items[c.name] = [];
    return { status: 201, body: c };
  });
  route(mock, "GET", `${V}/collections/{name}`, ({ params }) => { sync(); const c = col(params.name!); return c ? { status: 200, body: c } : nf(); });
  route(mock, "DELETE", `${V}/collections/{name}`, ({ params }) => {
    if (!col(params.name!)) return nf();
    st.collections = st.collections.filter((c) => c.name !== params.name);
    return { status: 204 };
  });
  route(mock, "POST", `${V}/collections/{name}/upsert`, ({ params, body }) => {
    if (!col(params.name!)) return nf();
    const rows = (st.items[params.name!] ??= []);
    const ids: string[] = [];
    let created = 0, updated = 0;
    for (const it of (body as { items: { id?: string; vectors?: Record<string, number[]>; metadata?: Record<string, unknown>; text?: string }[] }).items) {
      const id = it.id ?? `gen${rows.length + 1}`;
      const at = rows.findIndex((r) => r.id === id);
      const v = mk(id, it.vectors?.embedding ?? [0, 0, 0], it.metadata ?? {}, it.text ?? null);
      if (at >= 0) { rows[at] = v; updated++; } else { rows.push(v); created++; }
      ids.push(id);
    }
    return { status: 200, body: { ids, created, updated } };
  });
  route(mock, "GET", `${V}/collections/{name}/items/{id}`, ({ params }) => {
    const it = (st.items[params.name!] ?? []).find((r) => r.id === params.id);
    return it ? { status: 200, body: it } : envelope(404, "not_found", "no such item");
  });
  route(mock, "DELETE", `${V}/collections/{name}/items/{id}`, ({ params }) => {
    st.items[params.name!] = (st.items[params.name!] ?? []).filter((r) => r.id !== params.id);
    return { status: 204 };
  });
  const filtered = (name: string, filter?: string) => {
    const m = filter ? /^\s*(\w+)\s*=\s*"?([^"]*)"?\s*$/.exec(filter) : null;
    if (filter && !m) return undefined;
    return (st.items[name] ?? []).filter((r) => !m || String(r.metadata[m[1]!]) === m[2]);
  };
  route(mock, "POST", `${V}/collections/{name}/search`, ({ params, body }) => {
    const b = body as { vector: number[]; k?: number; filter?: string; exact?: boolean };
    if (!col(params.name!)) return nf();
    const rows = filtered(params.name!, b.filter);
    if (!rows) return envelope(400, "invalid_filter", `cannot parse filter ${b.filter}`);
    const hits = rows.map((r) => ({ id: r.id, distance: dist(b.vector, r.vectors.embedding!), score: 1 / (1 + dist(b.vector, r.vectors.embedding!)), metadata: r.metadata, text: r.text ?? undefined }))
      .sort((x, y) => x.distance - y.distance).slice(0, b.k ?? 10);
    return { status: 200, body: { hits, strategy: b.exact ? "exact" : b.filter ? "filtered_ann" : "ann", candidates: rows.length, took_ms: 0.42 } };
  });
  route(mock, "POST", `${V}/collections/{name}/hybrid`, ({ params, body }) => {
    const b = body as { vector?: number[]; text?: string; k?: number; filter?: string };
    if (!col(params.name!)) return nf();
    const rows = filtered(params.name!, b.filter);
    if (!rows) return envelope(400, "invalid_filter", "bad filter");
    const words = (b.text ?? "").toLowerCase().split(/\s+/).filter(Boolean);
    const hits = rows.map((r, i) => {
      const tm = words.filter((w) => (r.text ?? "").includes(w)).length;
      return { id: r.id, score: 0.03 - i * 0.001 + tm * 0.01, ...(b.vector ? { vector_rank: i + 1, vector_distance: dist(b.vector, r.vectors.embedding!) } : {}), ...(tm ? { text_rank: i + 1, text_score: tm * 1.5 } : {}) };
    }).sort((x, y) => y.score - x.score).slice(0, b.k ?? 10);
    return { status: 200, body: { hits, vector_strategy: b.vector ? "exact" : undefined, vector_candidates: b.vector ? rows.length : 0, text_candidates: words.length ? 2 : 0, took_ms: 1.1 } };
  });
  route(mock, "GET", `${V}/collections/{name}/stats`, ({ params }) => {
    const c = col(params.name!);
    return c ? { status: 200, body: { collection: c.name, items: (st.items[c.name] ?? []).length, items_with_text: 3, change_seq: 9, indexes: status(c) } } : nf();
  });
  route(mock, "POST", `${V}/collections/{name}/rebuild`, ({ params }) => {
    const c = col(params.name!);
    if (!c) return nf();
    st.rebuilds++;
    return { status: 202, body: { indexes: status(c) } };
  });
  route(mock, "POST", `${V}/collections/{name}/recall`, ({ params }) =>
    col(params.name!) ? { status: 200, body: { field: "embedding", k: 10, samples: 20, recall: 0.97, ann_ms: 0.3, exact_ms: 1.2, measured: NOW } } : nf());
  return st;
}

export interface GraphState { nodes: GraphNode[]; edges: GraphEdge[] }

export function installGraph(mock: MockApi): GraphState {
  const node = (id: string, label: string, properties: Record<string, unknown> = {}): GraphNode => ({ id, label, properties, created_at: NOW, updated_at: NOW });
  const edge = (id: string, type: string, source: string, target: string): GraphEdge => ({ id, type, source, target, directed: true, properties: {}, created_at: NOW, updated_at: NOW });
  const st: GraphState = {
    nodes: [node("alice", "Person", { age: 30 }), node("bob", "Person"), node("carol", "Person"), node("acme", "Company")],
    edges: [edge("e1", "KNOWS", "alice", "bob"), edge("e2", "KNOWS", "bob", "carol"), edge("e3", "WORKS_AT", "carol", "acme")],
  };
  const G = "/api/v1/graph";
  const byId = (id: string) => st.nodes.find((n) => n.id === id);
  const nodeNf = () => envelope(404, "node_not_found", "no such node");
  route(mock, "GET", `${G}/stats`, () => {
    const types = [...new Set(st.edges.map((e) => e.type))];
    return { status: 200, body: { nodes: st.nodes.length, edges: st.edges.length, edge_types: types.map((t) => ({ type: t, count: st.edges.filter((e) => e.type === t).length })) } };
  });
  route(mock, "GET", `${G}/nodes`, ({ query }) => {
    const label = query.get("label"), after = query.get("after"), limit = Number(query.get("limit") ?? 100);
    const all = st.nodes.filter((n) => !label || n.label === label).filter((n) => !after || n.id > after).sort((a, b) => (a.id < b.id ? -1 : 1));
    const items = all.slice(0, limit);
    return { status: 200, body: { items, ...(all.length > limit ? { next_after: items[items.length - 1]!.id } : {}) } };
  });
  route(mock, "POST", `${G}/nodes`, ({ body }) => {
    const b = body as { id?: string; label?: string; properties?: Record<string, unknown> };
    const id = b.id ?? `n${st.nodes.length + 1}`;
    if (byId(id)) return envelope(409, "node_exists", "a node with this id exists");
    const n = node(id, b.label ?? "", b.properties);
    st.nodes.push(n);
    return { status: 201, body: n };
  });
  route(mock, "GET", `${G}/nodes/{id}`, ({ params }) => { const n = byId(params.id!); return n ? { status: 200, body: n } : nodeNf(); });
  route(mock, "PUT", `${G}/nodes/{id}`, ({ params, body }) => {
    const n = byId(params.id!);
    if (!n) return nodeNf();
    const b = body as { label?: string; properties?: Record<string, unknown> };
    Object.assign(n, { label: b.label ?? n.label, properties: b.properties ?? n.properties });
    return { status: 200, body: n };
  });
  route(mock, "DELETE", `${G}/nodes/{id}`, ({ params }) => {
    if (!byId(params.id!)) return nodeNf();
    const gone = st.edges.filter((e) => e.source === params.id || e.target === params.id);
    st.edges = st.edges.filter((e) => !gone.includes(e));
    st.nodes = st.nodes.filter((n) => n.id !== params.id);
    return { status: 200, body: { id: params.id, deleted_edges: gone.length } };
  });
  route(mock, "GET", `${G}/nodes/{id}/neighbors`, ({ params, query }) => {
    if (!byId(params.id!)) return nodeNf();
    const dir = query.get("direction") ?? "out";
    const items: { direction: string; edge: GraphEdge; node: GraphNode }[] = [];
    for (const e of st.edges) {
      if ((dir === "out" || dir === "both") && e.source === params.id) items.push({ direction: "out", edge: e, node: byId(e.target)! });
      if ((dir === "in" || dir === "both") && e.target === params.id) items.push({ direction: "in", edge: e, node: byId(e.source)! });
    }
    return { status: 200, body: { items } };
  });
  route(mock, "GET", `${G}/edges`, ({ query }) => {
    const items = st.edges.filter((e) => (!query.get("type") || e.type === query.get("type")) && (!query.get("source") || e.source === query.get("source")) && (!query.get("target") || e.target === query.get("target")));
    return { status: 200, body: { items } };
  });
  route(mock, "POST", `${G}/edges`, ({ body }) => {
    const b = body as { type: string; source: string; target: string; directed?: boolean; properties?: Record<string, unknown> };
    if (!byId(b.source) || !byId(b.target)) return envelope(404, "node_not_found", "edge endpoint does not exist");
    const e = { ...edge(`e${st.edges.length + 1}`, b.type, b.source, b.target), directed: b.directed ?? true, properties: b.properties ?? {} };
    st.edges.push(e);
    return { status: 201, body: e };
  });
  route(mock, "PATCH", `${G}/edges/{id}`, ({ params, body }) => {
    const e = st.edges.find((x) => x.id === params.id);
    if (!e) return envelope(404, "edge_not_found", "no such edge");
    e.properties = { ...e.properties, ...((body as { properties?: Record<string, unknown> }).properties ?? {}) };
    return { status: 200, body: e };
  });
  route(mock, "DELETE", `${G}/edges/{id}`, ({ params }) => {
    st.edges = st.edges.filter((e) => e.id !== params.id);
    return { status: 204 };
  });
  route(mock, "POST", `${G}/traverse`, ({ body }) => {
    const b = body as { start: string; max_depth?: number; include_start?: boolean };
    if (!byId(b.start)) return nodeNf();
    const depth = new Map<string, number>([[b.start, 0]]);
    let frontier = [b.start];
    const edges = new Set<GraphEdge>();
    for (let d = 1; d <= (b.max_depth ?? 3); d++) {
      const nextF: string[] = [];
      for (const id of frontier) for (const e of st.edges) if (e.source === id) { edges.add(e); if (!depth.has(e.target)) { depth.set(e.target, d); nextF.push(e.target); } }
      frontier = nextF;
    }
    const nodes = [...depth].filter(([id]) => b.include_start || id !== b.start).map(([id, d]) => ({ depth: d, node: byId(id)! }));
    return { status: 200, body: { start: b.start, nodes, edges: [...edges], truncated: false } };
  });
  route(mock, "POST", `${G}/shortest-path`, ({ body }) => {
    const b = body as { from: string; to: string };
    if (!byId(b.from) || !byId(b.to)) return nodeNf();
    const prev = new Map<string, GraphEdge>();
    const seen = new Set([b.from]);
    const q = [b.from];
    while (q.length) {
      const id = q.shift()!;
      for (const e of st.edges) {
        const other = e.source === id ? e.target : e.target === id ? e.source : undefined; // direction "both"
        if (other && !seen.has(other)) { seen.add(other); prev.set(other, e); q.push(other); }
      }
    }
    if (!seen.has(b.to)) return { status: 200, body: { found: false, length: null, nodes: [], edges: [] } };
    const edges: GraphEdge[] = [];
    const ids = [b.to];
    for (let cur = b.to; cur !== b.from;) {
      const e = prev.get(cur)!;
      edges.unshift(e);
      cur = e.source === cur ? e.target : e.source;
      ids.unshift(cur);
    }
    return { status: 200, body: { found: true, length: edges.length, nodes: ids.map((i) => byId(i)!), edges } };
  });
  return st;
}
