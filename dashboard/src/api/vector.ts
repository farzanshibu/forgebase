/** Typed client for `/api/v1/vector` (ADR-0012). `VECTOR_OPERATIONS` is checked against /api/openapi.json in tests. */
import { request, send } from "./client";

const V = "/api/v1/vector";
const enc = encodeURIComponent;

export type Metric = "cosine" | "l2" | "dot";
export type Quantization = "f32" | "f16" | "i8";
export type FilterMode = "auto" | "pre" | "post";
export type IndexStateName = "unloaded" | "ready" | "building" | "failed";
export type Strategy = "ann" | "filtered_ann" | "post_filter" | "exact" | "exact_fallback";
export type Meta = Record<string, unknown>;

export interface FieldSpec {
  name: string;
  dimension: number;
  metric: Metric;
  quantization?: Quantization;
  connectivity?: number;
  expansion_add?: number;
  expansion_search?: number;
}
export interface VectorField {
  name: string; dimension: number; metric: Metric; quantization: Quantization;
  connectivity: number; expansion_add: number; expansion_search: number; created: string;
}
export interface NewVectorCollection { name: string; description?: string; fields?: FieldSpec[]; exact_threshold?: number }
export interface VectorCollection {
  name: string; description: string; fields: VectorField[]; exact_threshold: number; items: number; created: string; updated: string;
}
export interface UpsertItem { id?: string; vectors?: Record<string, number[]>; metadata?: Meta; text?: string }
export interface UpsertResult { ids: string[]; created: number; updated: number }
export interface VectorItem {
  id: string; vectors: Record<string, number[]>; metadata: Meta; text: string | null; version: number; created: string; updated: string;
}

export interface SearchRequest {
  field?: string; vector: number[]; k?: number; filter?: string; filter_mode?: FilterMode; exact?: boolean;
  include_vectors?: boolean; include_metadata?: boolean; include_text?: boolean;
}
export interface SearchHit { id: string; distance: number; score: number; metadata?: Meta; text?: string; vector?: number[] }
export interface SearchResponse { hits: SearchHit[]; strategy: Strategy; candidates?: number; took_ms: number }

export interface HybridRequest {
  field?: string; vector?: number[]; text?: string; text_operator?: "or" | "and"; k?: number; candidates?: number; rrf_k?: number;
  weights?: { vector?: number; text?: number }; filter?: string; filter_mode?: FilterMode;
  include_vectors?: boolean; include_metadata?: boolean; include_text?: boolean;
}
export interface HybridHit {
  id: string; score: number; vector_rank?: number; vector_distance?: number; text_rank?: number; text_score?: number;
  metadata?: Meta; text?: string; vector?: number[];
}
export interface HybridResponse { hits: HybridHit[]; vector_strategy?: Strategy; vector_candidates: number; text_candidates: number; took_ms: number }

export interface RecallReport { field: string; k: number; samples: number; recall: number; ann_ms: number; exact_ms: number; measured: string }
export interface IndexStatus {
  field: string; state: IndexStateName; serving: string; reason?: string; progress?: { done: number; total: number };
  canonical_vectors: number; indexed_vectors: number; memory_bytes: number; serialized_bytes: number; capacity: number;
  engine: string; engine_version: string; quantization: Quantization; connectivity: number; expansion_search: number; dirty: boolean;
  last_saved?: string; last_rebuild?: { reason: string; detail: string; vectors: number; duration_ms: number; finished: string };
  last_recall?: RecallReport; searches: number; ann_searches: number; exact_searches: number; mean_search_ms: number;
}
export interface CollectionStats { collection: string; items: number; items_with_text: number; change_seq: number; indexes: IndexStatus[] }
export interface VectorStats { collections: number; indexes: number; building: number; indexed_vectors: number; memory_bytes: number }

export const VECTOR_OPERATIONS = [
  "get /api/v1/vector/stats",
  "get /api/v1/vector/collections", "post /api/v1/vector/collections",
  "get /api/v1/vector/collections/{name}", "delete /api/v1/vector/collections/{name}",
  "get /api/v1/vector/collections/{name}/fields", "post /api/v1/vector/collections/{name}/fields",
  "delete /api/v1/vector/collections/{name}/fields/{field}",
  "post /api/v1/vector/collections/{name}/upsert", "post /api/v1/vector/collections/{name}/delete",
  "get /api/v1/vector/collections/{name}/items/{id}", "delete /api/v1/vector/collections/{name}/items/{id}",
  "post /api/v1/vector/collections/{name}/search", "post /api/v1/vector/collections/{name}/hybrid",
  "post /api/v1/vector/collections/{name}/rebuild", "get /api/v1/vector/collections/{name}/stats",
  "post /api/v1/vector/collections/{name}/recall",
] as const;

const c = (name: string) => `${V}/collections/${enc(name)}`;

export const vectorStats = (signal?: AbortSignal) => request<VectorStats>(`${V}/stats`, { signal });
export const listVectorCollections = async (signal?: AbortSignal) =>
  (await request<{ items: VectorCollection[] }>(`${V}/collections`, { signal })).items;
export const createVectorCollection = (def: NewVectorCollection) => send<VectorCollection>("POST", `${V}/collections`, def);
export const getVectorCollection = (name: string, signal?: AbortSignal) => request<VectorCollection>(c(name), { signal });
export const deleteVectorCollection = (name: string) => send<void>("DELETE", c(name));
export const addVectorField = (name: string, field: FieldSpec) => send<VectorField>("POST", `${c(name)}/fields`, field);
export const deleteVectorField = (name: string, field: string) => send<void>("DELETE", `${c(name)}/fields/${enc(field)}`);
export const upsertVectors = (name: string, items: UpsertItem[]) => send<UpsertResult>("POST", `${c(name)}/upsert`, { items });
export const deleteVectorItems = (name: string, ids: string[]) => send<{ deleted: number }>("POST", `${c(name)}/delete`, { ids });
export const getVectorItem = (name: string, id: string, signal?: AbortSignal) => request<VectorItem>(`${c(name)}/items/${enc(id)}`, { signal });
export const deleteVectorItem = (name: string, id: string) => send<void>("DELETE", `${c(name)}/items/${enc(id)}`);
export const vectorSearch = (name: string, body: SearchRequest) => send<SearchResponse>("POST", `${c(name)}/search`, body);
export const vectorHybrid = (name: string, body: HybridRequest) => send<HybridResponse>("POST", `${c(name)}/hybrid`, body);
export const rebuildVectorIndex = (name: string, field?: string) =>
  send<{ indexes: IndexStatus[] }>("POST", `${c(name)}/rebuild`, field ? { field } : {});
export const vectorCollectionStats = (name: string, signal?: AbortSignal) => request<CollectionStats>(`${c(name)}/stats`, { signal });
export const measureRecall = (name: string, body: { field?: string; samples?: number; k?: number }) =>
  send<RecallReport>("POST", `${c(name)}/recall`, body);
