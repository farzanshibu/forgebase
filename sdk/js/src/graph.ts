import type { RequestOptions, Transport } from "./http";
import type { Schemas } from "./types";

/*
 * Property graph (`/api/v1/graph`): nodes, typed edges, neighbours, traversal, shortest path and read-only
 * openCypher: `client.graph`.
 *
 * The generated schemas type `properties` as `Record<string, never>`; the aliases below widen it so a node
 * can be read and written with real values.
 */

/** A JSON object of node or edge properties (at most 64 KiB). */
export type GraphProperties = Record<string, unknown>;

type WithProps<T extends { properties: unknown }, P extends GraphProperties> = Omit<T, "properties"> & { properties: P };

/** A graph node as the server returns it. */
export type GraphNode<P extends GraphProperties = GraphProperties> = WithProps<Schemas["Node"], P>;
/** A typed edge between two nodes. */
export type GraphEdge<P extends GraphProperties = GraphProperties> = WithProps<Schemas["Edge"], P>;
/** Which way edges are followed: `out` (source to target, default), `in` or `both`. Undirected edges are walked both ways. */
export type GraphDirection = Schemas["Direction"];
/** A node one hop away, with the edge that connects it. `direction` is `out` when the queried node is the edge's source. */
export interface GraphNeighbor<P extends GraphProperties = GraphProperties> {
  direction: GraphDirection;
  edge: GraphEdge;
  node: GraphNode<P>;
}
/** A node reached by a traversal, with its fewest-hops depth. */
export interface TraversedNode<P extends GraphProperties = GraphProperties> {
  depth: number;
  node: GraphNode<P>;
}
export interface TraverseResult {
  start: string;
  /** Nearest first, then by id. */
  nodes: TraversedNode[];
  /** Edges between the returned nodes (and the start node). */
  edges: GraphEdge[];
  /** More nodes or edges matched than were returned. */
  truncated: boolean;
}
export interface PathResult {
  /** Whether a path within `maxDepth` exists. */
  found: boolean;
  /** Edges on the path. */
  length?: number | null;
  /** From `from` to `to`. */
  nodes: GraphNode[];
  /** Between consecutive nodes. */
  edges: GraphEdge[];
}
export type CypherStatus = Schemas["CypherStatus"];
export interface GraphStats {
  nodes: number;
  edges: number;
  /** Edge count per relationship type. */
  edge_types: { type: string; count: number }[];
  /** State of the derived openCypher graph (Apache AGE); absent when the engine has none. */
  cypher?: CypherStatus | null;
}
export interface CypherResult {
  /** One name per `RETURN` item (its alias, else its text). */
  columns: string[];
  /**
   * One JSON value per column. Vertices come back as `{id, label, properties}`, edges as
   * `{id, type, source, target, directed, properties}` and paths as `{nodes, edges}`, with ForgeBase ids.
   */
  rows: unknown[][];
  /** More rows matched than `limit`. */
  truncated: boolean;
}

export interface GraphPage<T> {
  items: T[];
  /** Pass as `after` (`offset` for neighbours) to continue; `null` on the last page. */
  nextAfter: string | null;
}

export interface NewNode<P extends GraphProperties = GraphProperties> {
  /** 1 to 128 of `A-Z a-z 0-9 _ . : @ -`; generated when absent. An existing id is a `ConflictError`. */
  id?: string;
  /** Kind, up to 64 characters (`User`, `Product`). */
  label?: string;
  properties?: P;
}

export interface NodeChange {
  /** New label (unchanged when absent). */
  label?: string;
  /** `replace`: the new properties. `patch`: a JSON merge patch (RFC 7396): `null` removes a key. */
  properties?: GraphProperties;
}

export interface ListNodesOptions {
  /** Only nodes with this label. */
  label?: string;
  /** Id of the last node of the previous page. */
  after?: string;
  /** 1 to 1000 (default 100). */
  limit?: number;
  signal?: AbortSignal;
}

export interface NeighborsOptions {
  /** `out` (default), `in` or `both`. */
  direction?: GraphDirection;
  /** Follow only these edge types. */
  types?: string[];
  /** 1 to 1000 (default 100). */
  limit?: number;
  /** Rows to skip: pass the previous page's `nextAfter`. */
  offset?: number;
  signal?: AbortSignal;
}

export interface NewEdge<P extends GraphProperties = GraphProperties> {
  /** Generated when absent. */
  id?: string;
  /** Relationship type, 1 to 64 of `A-Z a-z 0-9 _ . : -`. */
  type: string;
  /** Source node id; the node must exist. */
  source: string;
  /** Target node id; the node must exist. */
  target: string;
  /** `false` for an undirected relationship (default `true`). */
  directed?: boolean;
  properties?: P;
}

export interface ListEdgesOptions {
  type?: string;
  source?: string;
  target?: string;
  /** Id of the last edge of the previous page. */
  after?: string;
  /** 1 to 1000 (default 100). */
  limit?: number;
  signal?: AbortSignal;
}

/** Filter on the nodes a traversal returns (not on the walk): label and property equality, all must match. */
export interface NodeFilter {
  label?: string;
  /** Up to 8 equality conditions; values are strings, numbers or booleans. */
  properties?: Record<string, string | number | boolean>;
}

export interface TraverseOptions {
  /** Id of the start node. */
  start: string;
  /** `out` (default), `in` or `both`. */
  direction?: GraphDirection;
  /** Follow only these edge types (at most 20); empty = all. */
  edgeTypes?: string[];
  /** Hops, 1 to 10 (default 3). */
  maxDepth?: number;
  /** Most nodes returned, 1 to 1000 (default 100). */
  limit?: number;
  filter?: NodeFilter;
  /** Return the start node too, at depth 0 (default `false`). */
  includeStart?: boolean;
  /** Return the edges between the returned nodes (default `true`). */
  includeEdges?: boolean;
  signal?: AbortSignal;
}

export interface ShortestPathOptions {
  from: string;
  to: string;
  /** `out` (default), `in` or `both`. */
  direction?: GraphDirection;
  edgeTypes?: string[];
  /** Longest path in hops, 1 to 10 (default 6). */
  maxDepth?: number;
  signal?: AbortSignal;
}

export interface CypherOptions {
  /** Values of the `$name` parameters of the query. */
  params?: Record<string, unknown>;
  /** Most rows returned, 1 to 1000 (default 100). */
  limit?: number;
  /** Statement timeout, 1 to 30 000 ms (default 10 000). */
  timeoutMs?: number;
  signal?: AbortSignal;
}

const seg = encodeURIComponent;
const G = "/api/v1/graph";

interface WirePage<T> {
  items: T[];
  next_after?: string | null;
}
const page = <T>(w: WirePage<T>): GraphPage<T> => ({ items: w.items, nextAfter: w.next_after ?? null });

/** Nodes: `client.graph.nodes`. */
export class GraphNodesApi {
  constructor(private readonly t: Transport) {}

  /** Creates a node. */
  create<P extends GraphProperties = GraphProperties>(input: NewNode<P> = {}): Promise<GraphNode<P>> {
    return this.t.request(`${G}/nodes`, { method: "POST", body: { id: input.id, label: input.label, properties: input.properties } });
  }

  /** One page of nodes by id, optionally of one label. */
  async list<P extends GraphProperties = GraphProperties>(o: ListNodesOptions = {}): Promise<GraphPage<GraphNode<P>>> {
    return page(await this.t.request<WirePage<GraphNode<P>>>(`${G}/nodes`, { signal: o.signal, query: { label: o.label, after: o.after, limit: o.limit } }));
  }

  /** Every page, lazily: `for await (const p of fb.graph.nodes.pages({ label: "User" }))`. */
  async *pages<P extends GraphProperties = GraphProperties>(o: ListNodesOptions = {}): AsyncGenerator<GraphPage<GraphNode<P>>, void, undefined> {
    let after = o.after;
    for (;;) {
      const p = await this.list<P>({ ...o, after });
      yield p;
      if (!p.nextAfter) return;
      after = p.nextAfter;
    }
  }

  /** One node. A missing id is a `NotFoundError`. */
  get<P extends GraphProperties = GraphProperties>(id: string): Promise<GraphNode<P>> {
    return this.t.request(`${G}/nodes/${seg(id)}`);
  }

  /** `PUT`: replaces the properties (and the label when given). */
  replace<P extends GraphProperties = GraphProperties>(id: string, change: NodeChange): Promise<GraphNode<P>> {
    return this.t.request(`${G}/nodes/${seg(id)}`, { method: "PUT", body: { label: change.label, properties: change.properties } });
  }

  /** `PATCH`: merges the properties (RFC 7396: `null` removes a key). */
  patch<P extends GraphProperties = GraphProperties>(id: string, change: NodeChange): Promise<GraphNode<P>> {
    return this.t.request(`${G}/nodes/${seg(id)}`, { method: "PATCH", body: { label: change.label, properties: change.properties } });
  }

  /** Deletes a node and every edge that touches it. */
  delete(id: string): Promise<{ id: string; deleted_edges: number }> {
    return this.t.request(`${G}/nodes/${seg(id)}`, { method: "DELETE" });
  }

  /** Nodes one hop away with the connecting edge. `nextAfter` is the next `offset`. */
  async neighbors(id: string, o: NeighborsOptions = {}): Promise<GraphPage<GraphNeighbor>> {
    return page(
      await this.t.request<WirePage<GraphNeighbor>>(`${G}/nodes/${seg(id)}/neighbors`, {
        signal: o.signal,
        query: { direction: o.direction, type: o.types?.length ? o.types.join(",") : undefined, limit: o.limit, offset: o.offset },
      }),
    );
  }
}

/** Edges: `client.graph.edges`. */
export class GraphEdgesApi {
  constructor(private readonly t: Transport) {}

  /** Creates an edge between two existing nodes (a missing endpoint is a `NotFoundError`). */
  create<P extends GraphProperties = GraphProperties>(input: NewEdge<P>): Promise<GraphEdge<P>> {
    return this.t.request(`${G}/edges`, {
      method: "POST",
      body: { id: input.id, type: input.type, source: input.source, target: input.target, directed: input.directed, properties: input.properties },
    });
  }

  /** One page of edges by id, filtered by type and endpoints. */
  async list<P extends GraphProperties = GraphProperties>(o: ListEdgesOptions = {}): Promise<GraphPage<GraphEdge<P>>> {
    return page(
      await this.t.request<WirePage<GraphEdge<P>>>(`${G}/edges`, {
        signal: o.signal,
        query: { type: o.type, source: o.source, target: o.target, after: o.after, limit: o.limit },
      }),
    );
  }

  /** Every page, lazily. */
  async *pages<P extends GraphProperties = GraphProperties>(o: ListEdgesOptions = {}): AsyncGenerator<GraphPage<GraphEdge<P>>, void, undefined> {
    let after = o.after;
    for (;;) {
      const p = await this.list<P>({ ...o, after });
      yield p;
      if (!p.nextAfter) return;
      after = p.nextAfter;
    }
  }

  get<P extends GraphProperties = GraphProperties>(id: string): Promise<GraphEdge<P>> {
    return this.t.request(`${G}/edges/${seg(id)}`);
  }

  /** Merges properties (RFC 7396: `null` removes a key). Type and endpoints cannot change. */
  patch<P extends GraphProperties = GraphProperties>(id: string, properties: Record<string, unknown>): Promise<GraphEdge<P>> {
    return this.t.request(`${G}/edges/${seg(id)}`, { method: "PATCH", body: { properties } });
  }

  delete(id: string): Promise<void> {
    return this.t.request(`${G}/edges/${seg(id)}`, { method: "DELETE" });
  }
}

/** Graph helpers: `client.graph`. */
export class GraphApi {
  readonly nodes: GraphNodesApi;
  readonly edges: GraphEdgesApi;

  constructor(private readonly t: Transport) {
    this.nodes = new GraphNodesApi(t);
    this.edges = new GraphEdgesApi(t);
  }

  /** Multi-hop traversal: the nodes reachable from `start` within `maxDepth` hops (at most 10) with their fewest-hops depth, and the edges between them. Cycles are safe. */
  traverse(o: TraverseOptions): Promise<TraverseResult> {
    const filter = o.filter && { label: o.filter.label, properties: o.filter.properties };
    return this.t.request(`${G}/traverse`, {
      method: "POST",
      signal: o.signal,
      body: {
        start: o.start,
        direction: o.direction,
        edge_types: o.edgeTypes,
        max_depth: o.maxDepth,
        limit: o.limit,
        filter,
        include_start: o.includeStart,
        include_edges: o.includeEdges,
      },
    });
  }

  /** Shortest path by fewest hops (unweighted, breadth-first). `found: false` when no path exists within `maxDepth`. */
  shortestPath(o: ShortestPathOptions): Promise<PathResult> {
    return this.t.request(`${G}/shortest-path`, {
      method: "POST",
      signal: o.signal,
      body: { from: o.from, to: o.to, direction: o.direction, edge_types: o.edgeTypes, max_depth: o.maxDepth },
    });
  }

  /** Node and edge counts, edges per type, and the state of the openCypher graph. */
  stats(opts?: Pick<RequestOptions, "signal">): Promise<GraphStats> {
    return this.t.request(`${G}/stats`, opts);
  }

  /**
   * Read-only openCypher over the tenant's derived Apache AGE graph (needs `engine:graph_cypher` and PostgreSQL
   * with AGE). One query with a final `RETURN` (no `RETURN *`); parameters are `$name`.
   *
   * The first query of a tenant builds the graph: that call fails with a `ConflictError` whose code is
   * `graph_materialising` (`body.error.job_id` names the job); retry after it finishes (`stats().cypher.state === "ready"`).
   */
  cypher(query: string, o: CypherOptions = {}): Promise<CypherResult> {
    return this.t.request(`${G}/cypher`, {
      method: "POST",
      signal: o.signal,
      body: { query, params: o.params, limit: o.limit, timeout_ms: o.timeoutMs },
    });
  }

  /** Drops the derived openCypher graph (rebuilt on next use; the REST graph is untouched). */
  dropCypher(): Promise<{ dropped: boolean }> {
    return this.t.request(`${G}/cypher/drop`, { method: "POST" });
  }
}
