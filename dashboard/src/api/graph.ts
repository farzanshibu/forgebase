/** Typed client for `/api/v1/graph` (ADR-0006). `GRAPH_OPERATIONS` is checked against /api/openapi.json in tests. */
import { request, send } from "./client";

const G = "/api/v1/graph";
const enc = encodeURIComponent;

export type Direction = "out" | "in" | "both";
export type Props = Record<string, unknown>;

export interface GraphNode { id: string; label: string; properties: Props; created_at: string; updated_at: string }
export interface GraphEdge {
  id: string; type: string; source: string; target: string; directed: boolean; properties: Props; created_at: string; updated_at: string;
}
export interface Neighbor { direction: Direction; edge: GraphEdge; node: GraphNode }
export interface Page<T> { items: T[]; next_after?: string | null }
export type CypherState = "none" | "materialising" | "ready" | "stale" | "failed";
/** The derived openCypher graph (FB-163); present in the stats only where Apache AGE runs. */
export interface CypherStatus { state: CypherState; graph: string; job_id?: string | null; nodes: number; edges: number; done: number; error?: string | null }
export interface GraphStats { nodes: number; edges: number; edge_types: { type: string; count: number }[]; cypher?: CypherStatus | null }
export interface CypherQuery { query: string; params?: Record<string, unknown>; limit?: number; timeout_ms?: number }
export interface CypherResult { columns: string[]; rows: unknown[][]; truncated: boolean }
export interface NewNode { id?: string; label?: string; properties?: Props }
export interface NewEdge { id?: string; type: string; source: string; target: string; directed?: boolean; properties?: Props }
export interface TraverseQuery {
  start: string; direction?: Direction; edge_types?: string[]; max_depth?: number; limit?: number;
  filter?: { label?: string; properties?: Props }; include_start?: boolean; include_edges?: boolean;
}
export interface TraverseResult { start: string; nodes: { depth: number; node: GraphNode }[]; edges: GraphEdge[]; truncated: boolean }
export interface PathQuery { from: string; to: string; direction?: Direction; edge_types?: string[]; max_depth?: number }
export interface PathResult { found: boolean; length: number | null; nodes: GraphNode[]; edges: GraphEdge[] }

export const GRAPH_OPERATIONS = [
  "get /api/v1/graph/stats",
  "get /api/v1/graph/nodes", "post /api/v1/graph/nodes",
  "get /api/v1/graph/nodes/{id}", "put /api/v1/graph/nodes/{id}", "patch /api/v1/graph/nodes/{id}", "delete /api/v1/graph/nodes/{id}",
  "get /api/v1/graph/nodes/{id}/neighbors",
  "get /api/v1/graph/edges", "post /api/v1/graph/edges",
  "get /api/v1/graph/edges/{id}", "patch /api/v1/graph/edges/{id}", "delete /api/v1/graph/edges/{id}",
  "post /api/v1/graph/traverse", "post /api/v1/graph/shortest-path",
  "post /api/v1/graph/cypher", "post /api/v1/graph/cypher/drop",
] as const;

function qs(o: Record<string, string | number | undefined>): string {
  const p = new URLSearchParams();
  for (const [k, v] of Object.entries(o)) if (v !== undefined && v !== "") p.set(k, String(v));
  const s = p.toString();
  return s ? `?${s}` : "";
}

export const graphStats = (signal?: AbortSignal) => request<GraphStats>(`${G}/stats`, { signal });
export const listNodes = (q: { label?: string; after?: string; limit?: number }, signal?: AbortSignal) =>
  request<Page<GraphNode>>(`${G}/nodes${qs(q)}`, { signal });
export const getNode = (id: string, signal?: AbortSignal) => request<GraphNode>(`${G}/nodes/${enc(id)}`, { signal });
export const createNode = (n: NewNode) => send<GraphNode>("POST", `${G}/nodes`, n);
export const updateNode = (id: string, n: { label?: string; properties?: Props }) => send<GraphNode>("PUT", `${G}/nodes/${enc(id)}`, n);
export const deleteNode = (id: string) => send<{ id: string; deleted_edges: number }>("DELETE", `${G}/nodes/${enc(id)}`);
export const neighbors = (id: string, q: { direction?: Direction; type?: string; limit?: number; offset?: number } = {}, signal?: AbortSignal) =>
  request<Page<Neighbor>>(`${G}/nodes/${enc(id)}/neighbors${qs(q)}`, { signal });
export const listEdges = (q: { type?: string; source?: string; target?: string; after?: string; limit?: number }, signal?: AbortSignal) =>
  request<Page<GraphEdge>>(`${G}/edges${qs(q)}`, { signal });
export const createEdge = (e: NewEdge) => send<GraphEdge>("POST", `${G}/edges`, e);
export const updateEdge = (id: string, properties: Props) => send<GraphEdge>("PATCH", `${G}/edges/${enc(id)}`, { properties });
export const deleteEdge = (id: string) => send<void>("DELETE", `${G}/edges/${enc(id)}`);
export const traverse = (q: TraverseQuery) => send<TraverseResult>("POST", `${G}/traverse`, q);
export const shortestPath = (q: PathQuery) => send<PathResult>("POST", `${G}/shortest-path`, q);
export const cypher = (q: CypherQuery) => send<CypherResult>("POST", `${G}/cypher`, q);
export const dropCypherGraph = () => send<{ dropped: boolean }>("POST", `${G}/cypher/drop`);
