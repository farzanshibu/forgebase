/**
 * Typed client for `/api/v1/insights/*` (ADR-0036, docs/observability.md §5): statement statistics, usage and
 * estimated cost, function profiles and audit investigation. Administrators only.
 */
import { request, send } from "./client";

const I = "/api/v1/insights";
const enc = encodeURIComponent;

function qs(params: object): string {
  const q = new URLSearchParams();
  for (const [k, v] of Object.entries(params)) if (v !== undefined && v !== null && v !== "") q.set(k, String(v));
  const s = q.toString();
  return s ? `?${s}` : "";
}

/** `from` and `to` are RFC 3339 or `YYYY-MM-DD`; `to` is exclusive. */
export interface Range { from?: string; to?: string }

// Statements
export interface StatementStat {
  fingerprint: string; statement: string; area: string; backend: string;
  calls: number; errors: number; error_rate: number; writes: number; slow: number; rows: number;
  total_ms: number; mean_ms: number; min_ms: number; max_ms: number; p50_ms: number; p95_ms: number; p99_ms: number;
}
export interface StatementList { items: StatementStat[]; total: number; from: string; to: string; limit: number; offset: number }
export interface StatementPoint { at: string; seconds: number; calls: number; errors: number; mean_ms: number; p95_ms: number; max_ms: number }
export interface StatementDetail extends StatementStat { series: StatementPoint[] }
export interface WindowStat { calls: number; errors: number; error_rate: number; mean_ms: number; p50_ms: number; p95_ms: number; p99_ms: number }
export interface Regression {
  kind: "latency" | "errors"; fingerprint: string; statement: string; area: string; backend: string;
  recent: WindowStat; baseline: WindowStat; ratio: number; delta_ms: number; extra_ms: number;
}
export interface RegressionReport {
  recent_from: string; recent_to: string; baseline_from: string; baseline_to: string;
  min_ratio: number; min_calls: number; min_delta_ms: number; items: Regression[];
}
export interface SlowStatement {
  id: number; at: string; fingerprint: string; statement: string; area: string; backend: string;
  duration_ms: number; rows: number; error: boolean; write: boolean;
}
export interface SlowStatementPage { items: SlowStatement[]; next_cursor: number | null }
export interface InsightsStatus {
  enabled: boolean; slow_query_ms: number; flush_interval_secs: number; top_n: number; retention_days: number;
  fingerprints_in_memory: number; unflushed_since: string; stored_rows: number; stored_slow_statements: number;
}
export interface FlushReport { statements: number; slow_statements: number; slow_dropped: number; functions: number }

export type StatementSort = "total_time" | "calls" | "mean" | "max" | "p50" | "p95" | "p99" | "errors" | "rows" | "slow";
export const STATEMENT_SORTS: StatementSort[] = ["total_time", "calls", "mean", "max", "p50", "p95", "p99", "errors", "rows", "slow"];

export const insightsStatus = (signal?: AbortSignal) => request<InsightsStatus>(`${I}/status`, { signal });
/** Writes the statistics held in memory now. */
export const flushInsights = () => send<FlushReport>("POST", `${I}/flush`);
export const listStatements = (f: Range & { area?: string; backend?: string; q?: string; sort?: StatementSort; limit?: number; offset?: number } = {}, signal?: AbortSignal) =>
  request<StatementList>(`${I}/queries${qs(f)}`, { signal });
export const getStatement = (fingerprint: string, r: Range = {}, signal?: AbortSignal) => request<StatementDetail>(`${I}/queries/${enc(fingerprint)}${qs(r)}`, { signal });
export const listRegressions = (f: { window?: number; baseline?: number; until?: string; min_calls?: number; min_ratio?: number; min_delta_ms?: number; area?: string } = {}, signal?: AbortSignal) =>
  request<RegressionReport>(`${I}/queries/regressions${qs(f)}`, { signal });
export const listSlowStatements = (f: Range & { fingerprint?: string; area?: string; min_ms?: number; q?: string; cursor?: string; limit?: number } = {}, signal?: AbortSignal) =>
  request<SlowStatementPage>(`${I}/slow-queries${qs(f)}`, { signal });

// Usage and cost
export type UsageLevel = "environment" | "project" | "org" | "realm";
export const USAGE_LEVELS: UsageLevel[] = ["environment", "project", "org", "realm"];
export interface UsageMetricInfo { key: string; kind: "counter" | "gauge"; unit: string; description: string; source: string }
export interface MetricValue { kind: "counter" | "gauge"; value: number; peak: number | null }
export interface CostLine { metric: string; quantity: number; price: string | null; amount_minor: number | null; credits: number | null }
/** Always an estimate: the period priced as if it were one billing period. */
export interface CostEstimate { estimate: boolean; currency: string; amount_minor: number; amount: string; credits: number; lines: CostLine[] }
export interface UsageRow {
  level: string; id: string; name: string; realm: string | null; org: string | null; project: string | null;
  /** False for usage events that are not attributed to an environment. */
  attributed: boolean;
  metrics: Record<string, MetricValue>;
  cost: CostEstimate | null;
}
export interface UsageReport {
  from: string; to: string; group_by: UsageLevel; sort: string; total: number; limit: number; offset: number;
  currency: string | null; cost_available: boolean; estimate: boolean; items: UsageRow[];
}
/** `[metric, value]` per day: `[["2026-11-01", 42], …]`. */
export interface UsageSeries { scope: string; metrics: Record<string, [string, number][]> }
export interface UsagePeriod { period?: "day" | "month"; date?: string; from?: string; to?: string }

export const listUsageMetrics = async (signal?: AbortSignal) => (await request<{ items: UsageMetricInfo[] }>(`${I}/usage/metrics`, { signal })).items;
export const insightsUsage = (f: UsagePeriod & { group_by?: UsageLevel; metric?: string; sort?: string; realm?: string; org?: string; project?: string; currency?: string; limit?: number; offset?: number } = {}, signal?: AbortSignal) =>
  request<UsageReport>(`${I}/usage${qs(f)}`, { signal });
/** The top `n` (at most 100) by one metric key or `cost`, largest first. */
export const topUsage = (f: UsagePeriod & { metric: string; n?: number; group_by?: UsageLevel; currency?: string }, signal?: AbortSignal) =>
  request<UsageReport>(`${I}/usage/top${qs(f)}`, { signal });
export const usageSeries = (scope: string, r: Range = {}, signal?: AbortSignal) => request<UsageSeries>(`${I}/usage/series${qs({ scope, ...r })}`, { signal });
export const rollupUsage = (day?: string) => send<{ day: string; values: number }>("POST", `${I}/usage/rollup${qs({ day })}`);

// Functions
export interface HostBreakdown { op: string; count: number; errors: number; total_ms: number; mean_ms: number; max_ms: number; share: number }
export interface FunctionStat {
  function: string; version: number; invocations: number; errors: number; error_rate: number; timeouts: number; cold_starts: number;
  mean_ms: number; p50_ms: number; p95_ms: number; p99_ms: number; max_ms: number;
  fuel_mean: number; fuel_max: number; fuel_limit: number; fuel_utilisation: number;
  memory_max_bytes: number; memory_limit_bytes: number; host_calls_per_invocation: number; host: HostBreakdown[];
}
export interface FunctionStatList { items: FunctionStat[]; from: string; to: string }
export interface FunctionPoint { at: string; invocations: number; errors: number; cold_starts: number; mean_ms: number; p95_ms: number }
export interface FunctionDetail { function: string; versions: FunctionStat[]; series: FunctionPoint[] }
export interface HostCallEvent { op: string; at_us: number; dur_us: number; ok: boolean }
export interface InvocationProfile {
  cold_start: boolean; prepare_us: number; duration_us: number; fuel_used: number; fuel_limit: number;
  memory_peak_bytes: number; memory_limit_bytes: number; tenant?: string | null; host_calls: number;
  host: Record<string, { count: number; total_us: number; max_us: number; errors: number }>;
  /** The first 64 host calls. */
  timeline: HostCallEvent[];
  timeline_dropped: number;
}
export interface Invocation {
  id: number; function: string; version: number; trigger: string; status: string; duration_ms: number; fuel_used: number;
  error: string | null; logs: { ts?: string; level?: string; message?: string }[]; request_id: string | null; created_at: string; trace_id?: string | null;
  profile: InvocationProfile | null;
}
export interface InvocationPage { items: Invocation[]; next_cursor: number | null }

export const listFunctionStats = (f: Range & { function?: string; version?: number } = {}, signal?: AbortSignal) => request<FunctionStatList>(`${I}/functions${qs(f)}`, { signal });
export const getFunctionStats = (name: string, r: Range = {}, signal?: AbortSignal) => request<FunctionDetail>(`${I}/functions/${enc(name)}${qs(r)}`, { signal });
export const listInvocations = (f: { function?: string; version?: number; status?: string; trigger?: string; request_id?: string; errors?: boolean; min_duration_ms?: number; cursor?: string; limit?: number } = {}, signal?: AbortSignal) =>
  request<InvocationPage>(`${I}/functions/invocations${qs(f)}`, { signal });
export const getInvocation = (id: number | string, signal?: AbortSignal) => request<Invocation>(`${I}/functions/invocations/${enc(String(id))}`, { signal });

// Audit
export interface InsightAuditEntry {
  id: number; at: string; event: string; outcome: string; actor: string;
  resource_type: string | null; collection: string | null; resource_id: string | null;
  ip: string | null; user_agent: string | null; request_id: string | null; impersonator: string | null; detail: unknown;
}
export interface AuditPage { items: InsightAuditEntry[]; next_cursor: number | null }
export interface AuditRelated { event: InsightAuditEntry; same_request: InsightAuditEntry[]; same_actor: InsightAuditEntry[] }
export interface AuditCount { key: string; count: number }
export interface AuditSummary { total: number; outcomes: AuditCount[]; events: AuditCount[]; actors: AuditCount[]; ips: AuditCount[] }
export interface InsightAuditFilter extends Range {
  actor?: string; actor_type?: string; action?: string; resource_type?: string; collection?: string; resource_id?: string;
  outcome?: string; ip?: string; request_id?: string; q?: string; order?: "asc" | "desc";
}
export type AuditFormat = "csv" | "jsonl";

export const searchAudit = (f: InsightAuditFilter & { cursor?: string; limit?: number } = {}, signal?: AbortSignal) => request<AuditPage>(`${I}/audit${qs(f)}`, { signal });
export const getAuditEntry = (id: number, signal?: AbortSignal) => request<InsightAuditEntry>(`${I}/audit/${id}`, { signal });
export const auditRelated = (id: number, f: { window_secs?: number; limit?: number } = {}, signal?: AbortSignal) => request<AuditRelated>(`${I}/audit/${id}/related${qs(f)}`, { signal });
export const auditSummary = (r: Range = {}, signal?: AbortSignal) => request<AuditSummary>(`${I}/audit/summary${qs(r)}`, { signal });
/**
 * The URL of the audit export download (streamed CSV or JSON lines, oldest first). It is a plain link: the browser's
 * session cookie authorises it and the file streams to disk instead of through the page.
 */
export function auditExportUrl(f: InsightAuditFilter = {}, format: AuditFormat = "csv", maxRows?: number): string {
  return `${I}/audit/export${qs({ ...f, format, max_rows: maxRows })}`;
}

export const INSIGHTS_OPERATIONS = [
  "get /api/v1/insights/status", "post /api/v1/insights/flush", "get /api/v1/insights/queries", "get /api/v1/insights/queries/{fingerprint}",
  "get /api/v1/insights/queries/regressions", "get /api/v1/insights/slow-queries",
  "get /api/v1/insights/usage", "get /api/v1/insights/usage/top", "get /api/v1/insights/usage/metrics", "get /api/v1/insights/usage/series", "post /api/v1/insights/usage/rollup",
  "get /api/v1/insights/functions", "get /api/v1/insights/functions/{name}", "get /api/v1/insights/functions/invocations", "get /api/v1/insights/functions/invocations/{id}",
  "get /api/v1/insights/audit", "get /api/v1/insights/audit/{id}", "get /api/v1/insights/audit/{id}/related", "get /api/v1/insights/audit/summary", "get /api/v1/insights/audit/export",
] as const;
