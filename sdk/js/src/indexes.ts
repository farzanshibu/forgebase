import type { Transport } from "./http";
import type { Schemas } from "./types";

/*
 * Index management, planner statistics and the slow-query log (`/api/v1/indexes`, `/api/v1/slow-queries`): `client.indexes`.
 */

export type IndexInfo = Schemas["IndexInfoSchema"];
export type IndexStats = Schemas["IndexStatsSchema"];
export type IndexRecommendation = Schemas["IndexRecommendationSchema"];
export type SlowQuery = Schemas["SlowQuerySchema"];
export type SlowQueryLog = Schemas["SlowQueryLog"];

/** One key of an index. A bare string is an ascending field path. */
export type IndexKey = string | { path: string; desc?: boolean };

export interface CreateIndexInput {
  /** Collection name or id. */
  collection: string;
  /** Keys in order: field paths in filter syntax (`price`, `metadata.brand`, `title:lower`). For a full-text index, the fields to index. */
  keys: IndexKey[];
  /** Index name (generated when absent). */
  name?: string;
  unique?: boolean;
  /** Partial index condition, in filter syntax. */
  partial?: string;
  /** `btree` (default), `fulltext` or `trigram` (PostgreSQL with `pg_trgm`). */
  kind?: "btree" | "fulltext" | "trigram";
}

const enc = encodeURIComponent;
const I = "/api/v1/indexes";

const compact = <T extends Record<string, unknown>>(o: T): T => Object.fromEntries(Object.entries(o).filter(([, v]) => v !== undefined)) as T;

/** Indexes and the slow-query log: `client.indexes`. */
export class IndexesApi {
  constructor(private readonly t: Transport) {}

  /** Indexes with their statistics; `collection` limits the list to one collection. */
  async list(o: { collection?: string } = {}): Promise<IndexInfo[]> {
    return (await this.t.request<{ items: IndexInfo[] }>(I, { query: { collection: o.collection } })).items;
  }

  /** Creates an index (B-tree: single, composite, unique, partial, expression, JSON path; or full-text / trigram). A taken name or duplicate values for a unique index are a `ConflictError`. */
  create(input: CreateIndexInput): Promise<IndexInfo> {
    const body = compact({
      name: input.name,
      collection: input.collection,
      keys: input.keys.map((k) => (typeof k === "string" ? { path: k } : compact({ path: k.path, desc: k.desc }))),
      unique: input.unique,
      partial: input.partial,
      kind: input.kind,
    });
    return this.t.request(I, { method: "POST", body });
  }

  get(name: string): Promise<IndexInfo> {
    return this.t.request(`${I}/${enc(name)}`);
  }

  /** Drops a manual index (schema-derived ones follow their field). */
  async drop(name: string): Promise<void> {
    await this.t.request(`${I}/${enc(name)}`, { method: "DELETE" });
  }

  /** `REINDEX` + `ANALYZE`, or re-populates a full-text index. */
  rebuild(name: string): Promise<IndexInfo> {
    return this.t.request(`${I}/${enc(name)}/rebuild`, { method: "POST" });
  }

  /** Planner statistics, estimated rows and size on disk of one index. */
  stats(name: string): Promise<IndexStats> {
    return this.t.request(`${I}/${enc(name)}/stats`);
  }

  /** Indexes suggested by the slow-query log. */
  async recommendations(): Promise<IndexRecommendation[]> {
    return (await this.t.request<{ items: IndexRecommendation[] }>(`${I}/recommendations`)).items;
  }

  /** Refreshes planner statistics (`ANALYZE`), of one collection or of everything. */
  async analyze(o: { collection?: string } = {}): Promise<void> {
    await this.t.request(`${I}/analyze`, { method: "POST", body: compact({ collection: o.collection }) });
  }

  /** Recent statements slower than the threshold (SQL text only, never parameter values). */
  slowQueries(): Promise<SlowQueryLog> {
    return this.t.request("/api/v1/slow-queries");
  }

  async clearSlowQueries(): Promise<void> {
    await this.t.request("/api/v1/slow-queries", { method: "DELETE" });
  }

  /** Sets the slow-query threshold in milliseconds (`0` logs every statement); resolves with the new value. */
  async setSlowQueryThreshold(thresholdMs: number): Promise<number> {
    return (await this.t.request<{ threshold_ms: number }>("/api/v1/slow-queries/threshold", { method: "PUT", body: { threshold_ms: thresholdMs } })).threshold_ms;
  }
}
