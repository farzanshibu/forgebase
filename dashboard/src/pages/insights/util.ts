import type { InsightAuditFilter } from "../../api";
import { formatBytes } from "../../format";

export interface RangePreset { id: string; label: string; secs: number }
export const RANGES: RangePreset[] = [
  { id: "1h", label: "Last hour", secs: 3600 },
  { id: "6h", label: "Last 6 hours", secs: 6 * 3600 },
  { id: "24h", label: "Last 24 hours", secs: 86_400 },
  { id: "7d", label: "Last 7 days", secs: 7 * 86_400 },
];

/** The `from` and `to` (RFC 3339) of a preset, ending now. Read it when you fetch, not while rendering. */
export function rangeOf(id: string, now = Date.now()): { from: string; to: string } {
  const secs = (RANGES.find((r) => r.id === id) ?? RANGES[0]!).secs;
  return { from: new Date(now - secs * 1000).toISOString(), to: new Date(now).toISOString() };
}

export const AREAS = ["data", "auth", "jobs", "queue", "kv", "storage", "billing", "functions", "system"];

/** Milliseconds as `0.42 ms`, `12.3 ms` or `1.52 s`. */
export function fmtMs(ms: number | null | undefined): string {
  if (ms === null || ms === undefined || !Number.isFinite(ms)) return "—";
  if (ms >= 60_000) return `${(ms / 60_000).toFixed(1)} min`;
  if (ms >= 1000) return `${(ms / 1000).toFixed(2)} s`;
  if (ms >= 100) return `${Math.round(ms)} ms`;
  if (ms >= 1) return `${ms.toFixed(1)} ms`;
  return `${ms.toFixed(2)} ms`;
}

/** Microseconds in the same words. */
export const fmtUs = (us: number): string => fmtMs(us / 1000);

export const fmtInt = (n: number | null | undefined): string => (n === null || n === undefined ? "—" : n.toLocaleString());

/** `0.034` as `3.4%`. */
export const fmtRate = (r: number): string => `${(r * 100).toFixed(r > 0 && r < 0.001 ? 2 : 1).replace(/\.0$/, "")}%`;

/** A metric value in its unit: bytes as `1.2 MiB`, everything else as a plain number with the unit after it. */
export function fmtMetric(value: number, unit: string | undefined): string {
  if (unit === "bytes") return formatBytes(value);
  if (unit === "ms") return fmtMs(value);
  return `${value.toLocaleString()}${unit && unit !== "count" ? ` ${unit}` : ""}`;
}

/** A statement cut to one line for a table; the full text stays in the title. */
export const oneLine = (s: string, max = 120): string => {
  const t = s.replace(/\s+/g, " ").trim();
  return t.length > max ? `${t.slice(0, max - 1)}…` : t;
};

export const statementHref = (fingerprint: string): string => `/insights/queries/${encodeURIComponent(fingerprint)}`;

/** A `datetime-local` value (`2026-11-14T09:30`, the browser's local time) as an RFC 3339 UTC time; empty stays empty. */
export function localToIso(v: string): string | undefined {
  if (!v) return undefined;
  const d = new Date(v);
  return Number.isNaN(d.getTime()) ? undefined : d.toISOString();
}

export const today = (): string => new Date().toISOString().slice(0, 10);

/** The audit search form's fields as typed. */
export interface AuditDraft { actor: string; action: string; resource_type: string; resource_id: string; outcome: string; ip: string; request_id: string; from: string; to: string; q: string; order: "desc" | "asc" }
export const BLANK_AUDIT_DRAFT: AuditDraft = { actor: "", action: "", resource_type: "", resource_id: "", outcome: "", ip: "", request_id: "", from: "", to: "", q: "", order: "desc" };

/** The draft as the filter the API takes: empty fields are left out and the times become UTC. */
export function auditFilterOf(d: AuditDraft): InsightAuditFilter {
  const t = (v: string) => v.trim() || undefined;
  return {
    actor: t(d.actor), action: t(d.action), resource_type: t(d.resource_type), resource_id: t(d.resource_id), outcome: t(d.outcome), ip: t(d.ip), request_id: t(d.request_id),
    from: localToIso(d.from), to: localToIso(d.to), q: t(d.q), order: d.order === "asc" ? "asc" : undefined,
  };
}
