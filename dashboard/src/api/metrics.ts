/**
 * `GET /metrics` (Prometheus text exposition 0.0.4) parser and the derived figures the overview shows.
 * Pure functions only: the page polls, keeps a short history and calls these.
 */
import { request } from "./client";

export interface Sample {
  name: string;
  labels: Record<string, string>;
  value: number;
}

const NAME_RE = /^[a-zA-Z_:][a-zA-Z0-9_:]*/;

function parseValue(s: string): number {
  if (s === "+Inf" || s === "Inf") return Infinity;
  if (s === "-Inf") return -Infinity;
  return Number(s); // "NaN" -> NaN
}

/** Parse the label block starting after `{`; returns the labels and the index after `}`. */
function parseLabels(line: string, start: number): { labels: Record<string, string>; end: number } | null {
  const labels: Record<string, string> = {};
  let i = start;
  for (;;) {
    while (line[i] === " " || line[i] === ",") i++;
    if (line[i] === "}") return { labels, end: i + 1 };
    const eq = line.indexOf("=", i);
    if (eq < 0 || line[eq + 1] !== '"') return null;
    const key = line.slice(i, eq).trim();
    let j = eq + 2;
    let val = "";
    for (;;) {
      if (j >= line.length) return null;
      const c = line[j];
      if (c === "\\") {
        const n = line[j + 1];
        val += n === "n" ? "\n" : n;
        j += 2;
      } else if (c === '"') {
        j++;
        break;
      } else {
        val += c;
        j++;
      }
    }
    labels[key] = val;
    i = j;
  }
}

/** Parse Prometheus text. Comment lines and malformed lines are skipped; an optional trailing timestamp is ignored. */
export function parsePrometheus(text: string): Sample[] {
  const out: Sample[] = [];
  for (const raw of text.split("\n")) {
    const line = raw.trim();
    if (!line || line.startsWith("#")) continue;
    const m = NAME_RE.exec(line);
    if (!m) continue;
    const name = m[0];
    let labels: Record<string, string> = {};
    let i = name.length;
    if (line[i] === "{") {
      const p = parseLabels(line, i + 1);
      if (!p) continue;
      labels = p.labels;
      i = p.end;
    }
    const rest = line.slice(i).trim().split(/\s+/);
    if (!rest[0]) continue;
    const value = parseValue(rest[0]);
    if (Number.isNaN(value)) continue;
    out.push({ name, labels, value });
  }
  return out;
}

export const getMetrics = async (signal?: AbortSignal) => parsePrometheus(await request<string>("/metrics", { signal, text: true, headers: { Accept: "text/plain" } }));

/** Sum of every series called `name` (optionally only those matching `where`); `undefined` when no such series exists. */
export function sumOf(samples: Sample[], name: string, where?: (labels: Record<string, string>) => boolean): number | undefined {
  let found = false;
  let total = 0;
  for (const s of samples) {
    if (s.name !== name || (where && !where(s.labels))) continue;
    found = true;
    total += s.value;
  }
  return found ? total : undefined;
}

/** Cumulative histogram buckets summed over every label set: sorted `[upperBound, cumulativeCount]`. Empty when absent. */
export function histogramBuckets(samples: Sample[], name: string): [number, number][] {
  const by = new Map<number, number>();
  for (const s of samples) {
    if (s.name !== `${name}_bucket` || s.labels.le === undefined) continue;
    const le = parseValue(s.labels.le);
    if (Number.isNaN(le)) continue;
    by.set(le, (by.get(le) ?? 0) + s.value);
  }
  return [...by.entries()].sort((a, b) => a[0] - b[0]);
}

/**
 * Quantile `q` (0..1) of cumulative `buckets`, interpolating linearly inside the bucket like PromQL's histogram_quantile.
 * `undefined` when there are no observations.
 */
export function histogramQuantile(q: number, buckets: [number, number][]): number | undefined {
  if (buckets.length === 0) return undefined;
  const total = buckets[buckets.length - 1]?.[1];
  if (total === undefined || !(total > 0)) return undefined;
  const rank = q * total;
  let prevBound = 0;
  let prevCount = 0;
  for (const [bound, count] of buckets) {
    if (count >= rank) {
      if (!Number.isFinite(bound)) return prevBound; // above the last finite bucket: report its bound
      if (count === prevCount) return bound;
      return prevBound + (bound - prevBound) * ((rank - prevCount) / (count - prevCount));
    }
    prevBound = Number.isFinite(bound) ? bound : prevBound;
    prevCount = count;
  }
  return prevBound;
}

/** Per-bucket difference `now - before` (counter resets fall back to `now`). */
export function bucketDelta(now: [number, number][], before: [number, number][]): [number, number][] {
  const prev = new Map(before);
  const reset = now.some(([b, c]) => c < (prev.get(b) ?? 0));
  return now.map(([b, c]) => [b, reset ? c : c - (prev.get(b) ?? 0)]);
}

/** One poll of `/metrics` reduced to the figures the overview charts. A missing metric is `undefined`, never 0. */
export interface Reading {
  /** Milliseconds since the epoch when the scrape finished. */
  t: number;
  requests?: number;
  errors?: number;
  inFlight?: number;
  realtime?: number;
  resp?: number;
  dbSize?: number;
  walSize?: number;
  uptime?: number;
  rssBytes?: number;
  cpuSeconds?: number;
  buckets: [number, number][];
  functionRuns?: number;
}

export function readingFrom(samples: Sample[], t: number): Reading {
  const http = "forgebase_http_requests_total";
  return {
    t,
    requests: sumOf(samples, http),
    errors: sumOf(samples, http) === undefined ? undefined : sumOf(samples, http, (l) => /^5/.test(l.status ?? "")) ?? 0,
    inFlight: sumOf(samples, "forgebase_http_requests_in_flight"),
    realtime: sumOf(samples, "forgebase_realtime_connections"),
    resp: sumOf(samples, "forgebase_resp_connections"),
    dbSize: sumOf(samples, "forgebase_db_size_bytes"),
    walSize: sumOf(samples, "forgebase_db_wal_size_bytes"),
    uptime: sumOf(samples, "forgebase_uptime_seconds"),
    rssBytes: sumOf(samples, "process_resident_memory_bytes"),
    cpuSeconds: sumOf(samples, "process_cpu_seconds_total"),
    buckets: histogramBuckets(samples, "forgebase_http_request_duration_seconds"),
    functionRuns: sumOf(samples, "forgebase_function_executions_total"),
  };
}

/** A counter's per-second rate between two readings; `undefined` when either is missing, time did not advance, or it reset. */
export function ratePerSec(prev: number | undefined, now: number | undefined, dtMs: number): number | undefined {
  if (prev === undefined || now === undefined || dtMs <= 0 || now < prev) return undefined;
  return (now - prev) / (dtMs / 1000);
}

/** One chart point derived from two consecutive readings. */
export interface Point {
  t: number;
  rps?: number;
  /** Share of requests answered 5xx in the interval, 0..1. */
  errorRate?: number;
  p50?: number;
  p95?: number;
  cpuPct?: number;
  inFlight?: number;
  realtime?: number;
  rssBytes?: number;
}

export function pointFrom(prev: Reading | undefined, now: Reading): Point {
  const base: Point = { t: now.t, inFlight: now.inFlight, realtime: now.realtime, rssBytes: now.rssBytes };
  // Latency of the interval when there is one, else of the whole uptime so a quiet server still shows a figure.
  const window = prev ? bucketDelta(now.buckets, prev.buckets) : now.buckets;
  const useWindow = (window[window.length - 1]?.[1] ?? 0) > 0;
  const bk = useWindow ? window : now.buckets;
  base.p50 = histogramQuantile(0.5, bk);
  base.p95 = histogramQuantile(0.95, bk);
  if (!prev) return base;
  const dt = now.t - prev.t;
  base.rps = ratePerSec(prev.requests, now.requests, dt);
  const dReq = prev.requests !== undefined && now.requests !== undefined ? now.requests - prev.requests : undefined;
  const dErr = prev.errors !== undefined && now.errors !== undefined ? now.errors - prev.errors : undefined;
  if (dReq !== undefined && dErr !== undefined && dReq > 0 && dErr >= 0) base.errorRate = dErr / dReq;
  const cpu = ratePerSec(prev.cpuSeconds, now.cpuSeconds, dt);
  if (cpu !== undefined) base.cpuPct = cpu * 100;
  return base;
}

/** Append `p`, keeping at most `max` points. */
export function pushHistory(history: Point[], p: Point, max = 60): Point[] {
  const next = [...history, p];
  return next.length > max ? next.slice(next.length - max) : next;
}
