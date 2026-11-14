import { NetworkError } from "./errors";
import type { RequestOptions, Transport } from "./http";
import type { Schemas } from "./types";

/*
 * Operations insights (`/api/v1/insights`, ADR-0036): statement statistics by shape, query regressions and the slow
 * statement log, per-tenant usage with estimated cost, function profiles and invocations, and audit investigation with a
 * streamed CSV / JSONL export. `client.insights`.
 *
 * Administrator-only reads (`engine:read_insights` / `engine:read_audit`). Ranges are `from` / `to` (RFC 3339, a `Date`
 * or `YYYY-MM-DD`; `to` is exclusive). Response types are the server's wire models (snake_case).
 */

export type StatementStatus = Schemas["StatementStatus"];
export type FlushReport = Schemas["FlushReport"];
export type StatementList = Schemas["StatementList"];
export type StatementDetail = Schemas["StatementDetail"];
export type RegressionReport = Schemas["RegressionReport"];
export type SlowStatementPage = Schemas["SlowStatementPage"];
export type UsageReport = Schemas["UsageReport"];
export type UsageSeries = Schemas["UsageSeries"];
export type MetricList = Schemas["MetricList"];
export type RollupResult = Schemas["RollupResult"];
export type FunctionStatList = Schemas["FunctionStatList"];
export type FunctionDetail = Schemas["FunctionDetail"];
export type InvocationPage = Schemas["InvocationPage"];
export type InvocationDetail = Schemas["InvocationDetail"];
export type AuditPage = Schemas["AuditPageView"];
export type AuditEntry = Schemas["AuditEntryView"];
export type AuditRelated = Schemas["AuditRelatedView"];
export type AuditSummary = Schemas["AuditSummaryView"];

type Time = string | Date;

/** A time range: `from` inclusive, `to` exclusive. Defaults depend on the call (statistics: the hour before `to`; one shape, a function or slow queries: the day before `to`). */
export interface TimeRange {
  from?: Time;
  to?: Time;
}

export interface QueriesQuery extends TimeRange {
  /** `total_time` (default), `calls`, `mean`, `max`, `p50`, `p95`, `p99`, `errors`, `rows` or `slow`. */
  sort?: string;
  /** Only this subsystem (`data`, `jobs`, `queue`, `billing`, ...). */
  area?: string;
  /** `sqlite` or `postgres`. */
  backend?: string;
  /** Only statements containing this text. */
  q?: string;
  /** Default 50, at most 200. */
  limit?: number;
  offset?: number;
}

export interface RegressionsQuery {
  /** The recent window in seconds (default 3600). */
  window?: number;
  /** The baseline window before it, in seconds (default 86400). */
  baseline?: number;
  /** The window ends here (default: now). */
  until?: Time;
  /** Runs needed in each window (default 20). */
  minCalls?: number;
  /** Smallest p95 ratio against the baseline (default 2). */
  minRatio?: number;
  /** Smallest p95 growth in milliseconds (default 1). */
  minDeltaMs?: number;
  area?: string;
}

export interface SlowQueriesQuery extends TimeRange {
  fingerprint?: string;
  area?: string;
  /** At least this long. */
  minMs?: number;
  /** Only statements containing this text. */
  q?: string;
  /** The `next_cursor` of the previous page. */
  cursor?: number;
  /** Default 50, at most 500. */
  limit?: number;
}

export interface UsageQuery extends TimeRange {
  /** `day` or `month`. */
  period?: string;
  /** `YYYY-MM-DD`, or `YYYY-MM` for a month (default: today / this month). */
  date?: string;
  /** `environment` (default), `project`, `org` or `realm`. */
  groupBy?: string;
  /** Only these metrics (comma-separated metric keys). */
  metric?: string;
  /** A metric key or `cost`, largest first. */
  sort?: string;
  realm?: string;
  org?: string;
  project?: string;
  /** Currency of the estimated cost (ISO 4217). */
  currency?: string;
  /** Default 50, at most 500. */
  limit?: number;
  offset?: number;
}

export interface InvocationsQuery {
  function?: string;
  version?: number;
  /** `ok`, `failed`, `trap`, `fuel_exhausted`, `memory_limit`, `timeout`, ... */
  status?: string;
  /** `http`, `hook`, `job` or `test`. */
  trigger?: string;
  /** Executions of one request. */
  requestId?: string;
  /** Only executions that did not end `ok`. */
  errors?: boolean;
  minDurationMs?: number;
  cursor?: number;
  /** Default 50, at most 500. */
  limit?: number;
}

/** Filters of audit search and export. */
export interface AuditQuery extends TimeRange {
  /** Exactly this actor. */
  actor?: string;
  /** `user`, `admin`, `service_account`, `api_key` or `anonymous`. */
  actorType?: string;
  /** Events starting with this (`login`, `api_key.`). */
  action?: string;
  resourceType?: string;
  collection?: string;
  resourceId?: string;
  /** `success` or `failure`. */
  outcome?: string;
  /** Client address. */
  ip?: string;
  requestId?: string;
  /** Text in the details, event, resource id or user agent. */
  q?: string;
  /** `desc` (default for search) or `asc` (default for export). */
  order?: "asc" | "desc";
  /** The `next_cursor` of the previous page. */
  cursor?: number;
  /** Search only: page size (default 50, at most 1000). */
  limit?: number;
}

export type AuditExportFormat = "csv" | "jsonl";

export interface AuditExportOptions {
  format?: AuditExportFormat;
  /** At most this many entries (server default 100 000, at most 1 000 000). */
  maxRows?: number;
  signal?: AbortSignal;
}

/**
 * A streamed audit export. Read it once, one way: iterate it (`for await (const line of exp)` yields lines without their
 * line ends), `records()` for JSONL, `stream` for the raw bytes, or `text()` for everything. Nothing is buffered beyond
 * what the network delivers, so a large export stays small in memory.
 *
 * A CSV record can span lines when a field holds a line break; use a CSV parser on `stream` / `text()` if yours can.
 */
export class AuditExport implements AsyncIterable<string> {
  constructor(private readonly res: Response) {}

  /** `text/csv` or `application/x-ndjson`, as sent. */
  get contentType(): string | null {
    return this.res.headers.get("content-type");
  }

  /** The `Content-Disposition` file name suggestion, when sent. */
  get filename(): string | null {
    return /filename="?([^";]+)"?/i.exec(this.res.headers.get("content-disposition") ?? "")?.[1] ?? null;
  }

  /** The raw bytes as a web `ReadableStream`. */
  get stream(): ReadableStream<Uint8Array> {
    if (!this.res.body) throw new NetworkError("the export has no body");
    return this.res.body;
  }

  /** The whole export as text. */
  text(): Promise<string> {
    return this.res.text();
  }

  /** Lines, without the line end (`\n` or `\r\n`); an unterminated last line is yielded too; empty lines are kept. */
  async *lines(): AsyncGenerator<string> {
    const reader = this.stream.getReader();
    const dec = new TextDecoder();
    let buf = "";
    try {
      for (;;) {
        const { done, value } = await reader.read();
        if (done) break;
        buf += dec.decode(value, { stream: true });
        let i: number;
        while ((i = buf.indexOf("\n")) >= 0) {
          const line = buf.slice(0, i);
          buf = buf.slice(i + 1);
          yield line.endsWith("\r") ? line.slice(0, -1) : line;
        }
      }
      buf += dec.decode();
      if (buf) yield buf.endsWith("\r") ? buf.slice(0, -1) : buf;
    } finally {
      void reader.cancel().catch(() => undefined);
      reader.releaseLock();
    }
  }

  [Symbol.asyncIterator](): AsyncGenerator<string> {
    return this.lines();
  }

  /** JSONL exports: every line parsed. */
  async *records<T = AuditEntry>(): AsyncGenerator<T> {
    for await (const line of this.lines()) if (line.trim()) yield JSON.parse(line) as T;
  }
}

const seg = encodeURIComponent;
const I = "/api/v1/insights";
const iso = (v: Time | undefined): string | undefined => (v instanceof Date ? v.toISOString() : v);
const range = (r: TimeRange | undefined) => ({ from: iso(r?.from), to: iso(r?.to) });

const auditQuery = (q: AuditQuery) => ({
  ...range(q),
  actor: q.actor,
  actor_type: q.actorType,
  action: q.action,
  resource_type: q.resourceType,
  collection: q.collection,
  resource_id: q.resourceId,
  outcome: q.outcome,
  ip: q.ip,
  request_id: q.requestId,
  q: q.q,
  order: q.order,
});

/** Operations insights: `client.insights`. */
export class InsightsApi {
  constructor(private readonly t: Transport) {}

  /** State of the statement statistics and the slow-statement threshold. */
  status(): Promise<StatementStatus> {
    return this.t.request(`${I}/status`);
  }

  /** Writes the statistics held in memory to the database now (the server also does it every `[observability] flush_interval_secs`). */
  flush(): Promise<FlushReport> {
    return this.t.request(`${I}/flush`, { method: "POST", body: {} });
  }

  // ---------------------------------------------------------------- statements

  /** Statistics per statement shape (fingerprint) over a range: calls, errors, rows, total, mean and max time, p50 / p95 / p99. Values in the text are `?`. */
  queries(q: QueriesQuery = {}): Promise<StatementList> {
    return this.t.request(`${I}/queries`, { query: { ...range(q), sort: q.sort, area: q.area, backend: q.backend, q: q.q, limit: q.limit, offset: q.offset } });
  }

  /** One statement shape with its time series and example. */
  query(fingerprint: string, r: TimeRange = {}): Promise<StatementDetail> {
    return this.t.request(`${I}/queries/${seg(fingerprint)}`, { query: range(r) });
  }

  /** Statements whose p95 in the recent window is at least `minRatio` times their baseline, or whose error rate jumped. */
  regressions(q: RegressionsQuery = {}): Promise<RegressionReport> {
    return this.t.request(`${I}/queries/regressions`, {
      query: { window: q.window, baseline: q.baseline, until: iso(q.until), min_calls: q.minCalls, min_ratio: q.minRatio, min_delta_ms: q.minDeltaMs, area: q.area },
    });
  }

  /** Statements at or above `[observability] slow_query_ms`, newest first, with their text (values replaced by `?`); `next_cursor` continues. */
  slow(q: SlowQueriesQuery = {}): Promise<SlowStatementPage> {
    return this.t.request(`${I}/slow-queries`, {
      query: { ...range(q), fingerprint: q.fingerprint, area: q.area, min_ms: q.minMs, q: q.q, cursor: q.cursor, limit: q.limit },
    });
  }

  // --------------------------------------------------------------------- usage

  /** Usage per tenant over a day, a month or a custom range (at most 400 days), grouped as asked, with an **estimated** cost. */
  usage(q: UsageQuery = {}): Promise<UsageReport> {
    return this.t.request(`${I}/usage`, {
      query: {
        period: q.period, date: q.date, ...range(q), group_by: q.groupBy, metric: q.metric, sort: q.sort,
        realm: q.realm, org: q.org, project: q.project, currency: q.currency, limit: q.limit, offset: q.offset,
      },
    });
  }

  /** The top `n` (default 10, at most 100) tenants by one metric key, or by `cost`. */
  top(metric: string, o: { n?: number } & Pick<UsageQuery, "period" | "date" | "from" | "to" | "groupBy" | "currency"> = {}): Promise<UsageReport> {
    return this.t.request(`${I}/usage/top`, {
      query: { metric, n: o.n, period: o.period, date: o.date, ...range(o), group_by: o.groupBy, currency: o.currency },
    });
  }

  /** Daily values of one scope: `env:<environment id>` (default range: the 30 days before `to`). */
  series(scope: string, r: TimeRange = {}): Promise<UsageSeries> {
    return this.t.request(`${I}/usage/series`, { query: { scope, ...range(r) } });
  }

  /** The built-in usage metrics and what they mean. */
  metrics(): Promise<MetricList> {
    return this.t.request(`${I}/usage/metrics`);
  }

  /** Recomputes the resource and work numbers of a day from the live tables (`day`: `YYYY-MM-DD`, default today). */
  rollup(o: { day?: string } = {}): Promise<RollupResult> {
    return this.t.request(`${I}/usage/rollup`, { method: "POST", body: {}, query: { day: o.day } });
  }

  // ----------------------------------------------------------------- functions

  /** Statistics per function version over a range (default: the hour before `to`): executions, errors, timeouts, cold starts, duration percentiles, fuel and memory, host calls. */
  functions(q: TimeRange & { function?: string; version?: number } = {}): Promise<FunctionStatList> {
    return this.t.request(`${I}/functions`, { query: { ...range(q), function: q.function, version: q.version } });
  }

  /** One function: statistics per version and a time series (default range: the day before `to`). */
  function(name: string, r: TimeRange = {}): Promise<FunctionDetail> {
    return this.t.request(`${I}/functions/${seg(name)}`, { query: range(r) });
  }

  /** Recent executions with their profile (cold start, fuel, memory, host calls, log lines, outcome, trigger), newest first; `next_cursor` continues. */
  invocations(q: InvocationsQuery = {}): Promise<InvocationPage> {
    return this.t.request(`${I}/functions/invocations`, {
      query: {
        function: q.function, version: q.version, status: q.status, trigger: q.trigger, request_id: q.requestId,
        errors: q.errors, min_duration_ms: q.minDurationMs, cursor: q.cursor, limit: q.limit,
      },
    });
  }

  /** One execution with its profile. */
  invocation(id: string): Promise<InvocationDetail> {
    return this.t.request(`${I}/functions/invocations/${seg(id)}`);
  }

  // --------------------------------------------------------------------- audit

  /** Searches the audit trail, newest first by default; `next_cursor` continues the listing. */
  audit(q: AuditQuery = {}): Promise<AuditPage> {
    return this.t.request(`${I}/audit`, { query: { ...auditQuery(q), cursor: q.cursor, limit: q.limit } });
  }

  /** One audit entry. */
  auditEntry(id: string): Promise<AuditEntry> {
    return this.t.request(`${I}/audit/${seg(id)}`);
  }

  /** The entries of the same request, and of the same actor within `windowSecs` either side (default 300, at most 3600); `limit` per group (default 100, at most 500). */
  auditRelated(id: string, o: { windowSecs?: number; limit?: number } = {}): Promise<AuditRelated> {
    return this.t.request(`${I}/audit/${seg(id)}/related`, { query: { window_secs: o.windowSecs, limit: o.limit } });
  }

  /** Counts of the audit trail in a range: per outcome, and the top events, actors and client addresses. */
  auditSummary(r: TimeRange = {}): Promise<AuditSummary> {
    return this.t.request(`${I}/audit/summary`, { query: range(r) });
  }

  /**
   * Exports the entries that match as CSV (default) or JSONL, oldest first unless `order: "desc"`, streamed in pages so a
   * large export never sits in memory: iterate the result for lines, `records()` for parsed JSONL, `stream` for bytes.
   * In CSV, cells that start like a formula are prefixed with `'`. A stream that breaks half-way is not resumed: start again
   * with a `from` after the last row you have.
   */
  async exportAudit(q: AuditQuery = {}, o: AuditExportOptions = {}): Promise<AuditExport> {
    const opts: RequestOptions = {
      query: { ...auditQuery(q), format: o.format ?? "csv", max_rows: o.maxRows },
      headers: { Accept: o.format === "jsonl" ? "application/x-ndjson, application/json" : "text/csv" },
      signal: o.signal,
    };
    return new AuditExport(await this.t.send(`${I}/audit/export`, opts));
  }
}
