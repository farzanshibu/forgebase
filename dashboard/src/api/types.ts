/** Wire types for the ForgeBase API (docs/PLAN.md section 5). */
import type { TlsStatus } from "./capabilities";

export interface ApiErrorBody {
  error: { code: string; message: string; request_id?: string };
}

export interface HealthResponse {
  status: string;
}

export interface ReadyResponse {
  status: string;
  checks: Record<string, string>;
}

export interface SystemInfo {
  name: string;
  version: string;
  mode: string;
  data_dir: string;
  started_at: string;
  uptime_secs: number;
  database: {
    path: string;
    size_bytes: number;
    journal_mode: string;
    schema_version: number;
  };
  /** The certificate section (ADR-0033); absent on servers older than 0.3.4. */
  tls?: TlsStatus;
}

/** One entry of `error.details.fields` (400 validation errors). */
export interface FieldError {
  code: string;
  message: string;
}

/** Studio types: mirror `crates/forgebase-api/src/v1/schemas.rs` (checked against /api/openapi.json in tests). */
export type FieldType =
  | "text" | "number" | "bool" | "date" | "email" | "url" | "select" | "json"
  | "array" | "relation" | "file" | "password" | "geo" | "vector";

export const FIELD_TYPES: FieldType[] = [
  "text", "number", "bool", "date", "email", "url", "select", "json", "array", "relation", "file", "password", "geo", "vector",
];

export interface FieldDef {
  id?: string;
  name: string;
  type: FieldType | string;
  required?: boolean;
  unique?: boolean;
  hidden?: boolean;
  system?: boolean;
  default?: unknown;
  /** Type options (`min_length`, `values`, ...) sit beside `type` on the wire. */
  [option: string]: unknown;
}

export interface CollectionDef {
  id?: string;
  name: string;
  type: "base" | "auth" | "view" | string;
  system?: boolean;
  schema_mode?: "strict" | "flexible" | string;
  versioned?: boolean;
  fields: FieldDef[];
  view_query?: string | null;
  options?: Record<string, unknown>;
  version?: number;
  created?: string;
  updated?: string;
}

export type RecordObject = Record<string, unknown> & { id?: string; _version?: number };

export interface ListResponse {
  items: RecordObject[];
  page?: number | null;
  per_page: number;
  total_items?: number | null;
  total_pages?: number | null;
  next_cursor?: string | null;
}

export interface ListParams {
  filter?: string;
  sort?: string;
  page?: number;
  per_page?: number;
  search?: string;
  /** Trigram match of `search` (PostgreSQL with pg_trgm; FB-166). */
  fuzzy?: boolean;
  /** 0 to 1; the server's default is 0.3. */
  fuzzy_threshold?: number;
  fields?: string[];
}

export interface IndexKey { path: string; desc?: boolean }
export interface IndexDefinition {
  name?: string | null;
  collection: string;
  keys: IndexKey[];
  unique?: boolean;
  partial?: string | null;
  kind?: "btree" | "fulltext" | string;
}
export interface IndexStats { stat?: string | null; rows_estimate?: number | null; size_bytes?: number | null }
export interface IndexInfo {
  name: string;
  table: string;
  collection?: string | null;
  origin: string;
  kind: string;
  unique: boolean;
  partial: boolean;
  columns: string[];
  definition?: IndexDefinition | null;
  sql?: string | null;
  stats: IndexStats;
}
export interface IndexRecommendation {
  definition: IndexDefinition;
  reason: string;
  occurrences: number;
  max_duration_ms: number;
}

export interface PlanStep { id: number; parent: number; detail: string }
export interface QueryPlan { sql: string; steps: PlanStep[]; indexes_used: string[]; full_scans: string[] }

export interface SqlResult {
  kind: string;
  columns: string[];
  rows: unknown[][];
  rows_affected: number;
  last_insert_id?: number | null;
  truncated: boolean;
  duration_ms: number;
}

export interface TableInfo {
  name: string;
  kind: string;
  collection?: string | null;
  columns: { name: string; declared_type: string; not_null: boolean; default?: string | null; primary_key: boolean }[];
  indexes: string[];
  sql?: string | null;
}

export interface SlowQuery {
  sql: string;
  param_count: number;
  duration_ms: number;
  at: string;
  rows: number;
  label?: { collection: string; equality_keys: string[]; range_keys: string[]; sort_keys: string[] } | null;
}
export interface SlowQueryLog { threshold_ms: number; items: SlowQuery[] }
export interface ImportResult { imported: number; format: string }
export type ExportFormat = "json" | "ndjson" | "csv";
