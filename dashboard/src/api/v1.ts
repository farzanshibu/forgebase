/** Typed client for `/api/v1` (Database Studio). `OPERATIONS` is checked against /api/openapi.json in tests. */
import { request, send } from "./client";
import type {
  CollectionDef, ExportFormat, ImportResult, IndexDefinition, IndexInfo, IndexRecommendation, IndexStats, ListParams,
  ListResponse, QueryPlan, RecordObject, SlowQueryLog, SqlResult, TableInfo,
} from "./types";

const V1 = "/api/v1";
const enc = encodeURIComponent;

/** Every `method path` the client calls, as written in the OpenAPI document. */
export const OPERATIONS = [
  "get /api/v1/collections", "post /api/v1/collections",
  "get /api/v1/collections/{collection}", "put /api/v1/collections/{collection}", "delete /api/v1/collections/{collection}",
  "get /api/v1/collections/{collection}/history",
  "get /api/v1/collections/{collection}/records", "post /api/v1/collections/{collection}/records",
  "post /api/v1/collections/{collection}/records/query", "get /api/v1/collections/{collection}/records/count",
  "get /api/v1/collections/{collection}/records/{id}", "patch /api/v1/collections/{collection}/records/{id}",
  "put /api/v1/collections/{collection}/records/{id}", "delete /api/v1/collections/{collection}/records/{id}",
  "post /api/v1/collections/{collection}/records/explain",
  "get /api/v1/collections/{collection}/export", "post /api/v1/collections/{collection}/import",
  "get /api/v1/indexes", "post /api/v1/indexes", "get /api/v1/indexes/recommendations", "post /api/v1/indexes/analyze",
  "delete /api/v1/indexes/{name}", "post /api/v1/indexes/{name}/rebuild", "get /api/v1/indexes/{name}/stats",
  "get /api/v1/slow-queries", "delete /api/v1/slow-queries", "put /api/v1/slow-queries/threshold",
  "post /api/v1/sql", "post /api/v1/sql/explain", "get /api/v1/sql/tables",
] as const;

function listQuery(q: ListParams): string {
  const p = new URLSearchParams();
  if (q.filter) p.set("filter", q.filter);
  if (q.sort) p.set("sort", q.sort);
  if (q.search) p.set("search", q.search);
  if (q.search && q.fuzzy) {
    p.set("fuzzy", "true");
    if (q.fuzzy_threshold !== undefined) p.set("fuzzy_threshold", String(q.fuzzy_threshold));
  }
  if (q.fields?.length) p.set("fields", q.fields.join(","));
  p.set("page", String(q.page ?? 1));
  p.set("per_page", String(q.per_page ?? 25));
  return p.toString();
}

const col = (c: string) => `${V1}/collections/${enc(c)}`;

export const listCollections = async (signal?: AbortSignal) =>
  (await request<{ items: CollectionDef[] }>(`${V1}/collections`, { signal })).items;
export const createCollection = (def: CollectionDef) => send<CollectionDef>("POST", `${V1}/collections`, def);
export const updateCollection = (name: string, def: CollectionDef) => send<CollectionDef>("PUT", col(name), def);
export const deleteCollection = (name: string) => send<void>("DELETE", col(name));

export const listRecords = (c: string, q: ListParams, signal?: AbortSignal) =>
  request<ListResponse>(`${col(c)}/records?${listQuery(q)}`, { signal });
export const getRecord = (c: string, id: string, signal?: AbortSignal) =>
  request<RecordObject>(`${col(c)}/records/${enc(id)}`, { signal });
export const createRecord = (c: string, rec: Record<string, unknown>) => send<RecordObject>("POST", `${col(c)}/records`, rec);
const ifMatch = (v?: number) => (v === undefined ? undefined : { "If-Match": `"${v}"` });
export const patchRecord = (c: string, id: string, patch: Record<string, unknown>, version?: number) =>
  send<RecordObject>("PATCH", `${col(c)}/records/${enc(id)}`, patch, ifMatch(version));
export const replaceRecord = (c: string, id: string, rec: Record<string, unknown>, version?: number) =>
  send<RecordObject>("PUT", `${col(c)}/records/${enc(id)}`, rec, ifMatch(version));
export const deleteRecord = (c: string, id: string) => send<void>("DELETE", `${col(c)}/records/${enc(id)}`);

export const explainRecords = (c: string, q: { filter?: string; sort?: string }) =>
  send<QueryPlan>("POST", `${col(c)}/records/explain`, q);

export const exportRecords = (c: string, format: ExportFormat, filter?: string) => {
  const p = new URLSearchParams({ format });
  if (filter) p.set("filter", filter);
  return request<string>(`${col(c)}/export?${p}`, { text: true });
};
export const importRecords = (c: string, format: ExportFormat, body: string) =>
  request<ImportResult>(`${col(c)}/import?format=${format}`, {
    method: "POST",
    rawBody: body,
    headers: { "Content-Type": "application/octet-stream" },
  });

export const listIndexes = async (collection?: string, signal?: AbortSignal) =>
  (await request<{ items: IndexInfo[] }>(`${V1}/indexes${collection ? `?collection=${enc(collection)}` : ""}`, { signal })).items;
export const createIndex = (def: IndexDefinition) => send<IndexInfo>("POST", `${V1}/indexes`, def);
export const dropIndex = (name: string) => send<void>("DELETE", `${V1}/indexes/${enc(name)}`);
export const rebuildIndex = (name: string) => send<IndexInfo>("POST", `${V1}/indexes/${enc(name)}/rebuild`);
export const indexStats = (name: string) => request<IndexStats>(`${V1}/indexes/${enc(name)}/stats`);
export const analyzeIndexes = (collection?: string) => send<void>("POST", `${V1}/indexes/analyze`, { collection });
export const indexRecommendations = async (signal?: AbortSignal) =>
  (await request<{ items: IndexRecommendation[] }>(`${V1}/indexes/recommendations`, { signal })).items;

export const slowQueries = (signal?: AbortSignal) => request<SlowQueryLog>(`${V1}/slow-queries`, { signal });
export const clearSlowQueries = () => send<void>("DELETE", `${V1}/slow-queries`);
export const setSlowThreshold = (threshold_ms: number) =>
  send<{ threshold_ms: number }>("PUT", `${V1}/slow-queries/threshold`, { threshold_ms });

export const runSql = (sql: string) => send<SqlResult>("POST", `${V1}/sql`, { sql });
export const explainSql = (sql: string) => send<QueryPlan>("POST", `${V1}/sql/explain`, { sql });
export const sqlTables = async (signal?: AbortSignal) =>
  (await request<{ items: TableInfo[] }>(`${V1}/sql/tables`, { signal })).items;
