/**
 * Server health: version, uptime and the live metric cards, polled every five seconds (audit A-02, A-16).
 *
 * Usage:
 *   <ServerHealth />
 *
 * A card the server does not report is not drawn: it collapses into one muted line under the grid. A metric counts
 * as reported while it appeared in this poll or the one before, so one missing scrape does not make a card jump.
 * "collecting…" shows only while a reported series has fewer than two points.
 */
import { useId, useRef } from "react";
import { useOptionalSession } from "../../auth/session";
import { ApiError, getMetrics, getSystemInfo, jobStats, kvInfo, pointFrom, pushHistory, readingFrom, storageQuotaFor, storageStats, vectorStats } from "../../api";
import type { JobStats, KvStats, Point, Reading, StorageStats, SystemInfo, VectorStats } from "../../api";
import { formatBytes, formatUptime } from "../../format";
import { usePolling } from "../../hooks";
import { explain } from "../../errors";
import { Activity01Icon, AiNetworkIcon, Alert02Icon, Clock01Icon, CpuIcon, Database01Icon, Folder02Icon, Plug01Icon, RefreshIcon, TaskDaily01Icon, ZapIcon } from "@hugeicons/core-free-icons";
import { HugeiconsIcon, type IconSvgElement } from "@hugeicons/react";
import { Badge } from "panelui-web/badge";
import { Card as PanelCard } from "panelui-web/card";
import { Stat as PanelStat } from "panelui-web/stat";
import { Button } from "../../components/ui";
import { ErrorPanel } from "../../components/ErrorPanel";
import { SkeletonCards } from "../../components/Skeleton";
import { DocsLink } from "../../components/PageHeader";

export const OVERVIEW_REFRESH_MS = 5000;
const HISTORY = 60;

/** One source of a card: a value, an absent answer (404/501, or a switched-off feature), no permission, or a failure. */
type Part<T> = { state: "ok"; value: T } | { state: "absent" } | { state: "denied" } | { state: "error"; error: unknown };

async function part<T>(f: () => Promise<T>): Promise<Part<T>> {
  try {
    return { state: "ok", value: await f() };
  } catch (e) {
    if (e instanceof DOMException && e.name === "AbortError") throw e;
    if (e instanceof ApiError) {
      if (e.status === 404 || e.status === 501 || e.code === "feature_disabled") return { state: "absent" };
      if (e.isAuth) return { state: "denied" };
    }
    return { state: "error", error: e };
  }
}

/** Raw metrics a card can show, keyed for "reported in this poll or the one before". */
const METRIC_KEYS = ["requests", "errors", "inFlight", "realtime", "rssBytes", "cpuSeconds", "latency", "dbSize"] as const;
type MetricKey = (typeof METRIC_KEYS)[number];

function presentIn(r: Reading | undefined): Set<MetricKey> {
  const out = new Set<MetricKey>();
  if (!r) return out;
  for (const k of METRIC_KEYS) {
    if (k === "latency") {
      if (r.buckets.length > 0) out.add(k);
    } else if (r[k] !== undefined) out.add(k);
  }
  return out;
}

interface Snapshot {
  reading: Part<Reading>;
  /** Metrics in this poll or the previous one. */
  reported: Set<MetricKey>;
  history: Point[];
  info: Part<SystemInfo>;
  storage: Part<{ stats: StorageStats; quota: number | null }>;
  vector: Part<VectorStats>;
  jobs: Part<JobStats>;
  kv: Part<KvStats>;
}

const num = (n: number | undefined, digits = 1) => (n === undefined ? undefined : n.toFixed(digits));
const ms = (s: number | undefined) => (s === undefined ? undefined : s < 1 ? `${(s * 1000).toFixed(s < 0.01 ? 1 : 0)} ms` : `${s.toFixed(2)} s`);
const pct = (n: number | undefined) => (n === undefined ? undefined : `${n.toFixed(n < 10 ? 1 : 0)}%`);

function Card({ title, children, icon }: { title: string; children: React.ReactNode; icon: IconSvgElement }) {
  return (
    <PanelCard as="section" size="sm" aria-label={title} className="rounded-xl">
      <PanelCard.Header className="pb-2">
        <PanelCard.Title as="h3" className="flex items-center gap-2 text-sm font-medium text-muted-foreground">
          <span className="flex size-6 items-center justify-center rounded-md bg-primary/10 text-primary" aria-hidden="true">
            <HugeiconsIcon icon={icon} size={14} strokeWidth={2} />
          </span>
          {title}
        </PanelCard.Title>
      </PanelCard.Header>
      <PanelCard.Content>{children}</PanelCard.Content>
    </PanelCard>
  );
}

/** Why a card has no figure: no permission, or a failed call in plain words. */
function Problem({ p }: { p: Part<unknown> }) {
  if (p.state === "denied") return <p className="text-sm text-warning">Your account cannot read this.</p>;
  if (p.state === "error") return <p className="text-sm text-destructive">{explain(p.error).title}</p>;
  return null;
}

/** A big figure. A reported metric without a value yet (a rate needs two readings) shows a dash. */
function Stat({ label, value, sub }: { label: string; value: string | undefined; sub?: string }) {
  return (
    <PanelStat.Content>
      <PanelStat.Label className="text-xs">{label}</PanelStat.Label>
      <PanelStat.Value size="sm" className="tabular-nums tracking-tight">{value ?? "—"}</PanelStat.Value>
      {value !== undefined && sub && <p className="text-xs text-muted-foreground">{sub}</p>}
    </PanelStat.Content>
  );
}

/** Tiny inline-SVG time series; points with an undefined value leave gaps. */
function Sparkline({ label, values }: { label: string; values: (number | undefined)[] }) {
  const defined = values.filter((v): v is number => v !== undefined);
  if (defined.length < 2) return <p className="mt-2 text-xs text-muted-foreground">collecting…</p>;
  const w = 200;
  const h = 40;
  const max = Math.max(...defined, 1e-9);
  const min = Math.min(...defined, 0);
  const span = max - min || 1;
  const step = w / Math.max(values.length - 1, 1);
  // Runs of defined points; an undefined value breaks the line.
  const runs: [number, number][][] = [];
  let cur: [number, number][] = [];
  values.forEach((v, i) => {
    if (v === undefined) {
      if (cur.length) runs.push(cur);
      cur = [];
      return;
    }
    cur.push([i * step, h - ((v - min) / span) * (h - 2) - 1]);
  });
  if (cur.length) runs.push(cur);
  const line = (r: [number, number][]) => r.map(([x, y], i) => `${i ? "L" : "M"}${x.toFixed(1)},${y.toFixed(1)}`).join("");
  const area = (r: [number, number][]) => `${line(r)}L${r[r.length - 1]![0].toFixed(1)},${h}L${r[0]![0].toFixed(1)},${h}Z`;
  return (
    <svg role="img" aria-label={label} viewBox={`0 0 ${w} ${h}`} className="mt-3 h-10 w-full text-primary" preserveAspectRatio="none">
      {runs.map((r, i) => (
        <path key={`area${i}`} d={area(r)} fill="currentColor" opacity="0.1" stroke="none" />
      ))}
      {runs.map((r, i) => (
        <path key={i} d={line(r)} fill="none" stroke="currentColor" strokeWidth="1.75" strokeLinejoin="round" vectorEffect="non-scaling-stroke" />
      ))}
    </svg>
  );
}

function Meter({ label, used, limit }: { label: string; used: number; limit: number }) {
  const p = limit > 0 ? Math.min(100, Math.round((used / limit) * 100)) : 0;
  return (
    <div role="progressbar" aria-label={label} aria-valuenow={p} aria-valuemin={0} aria-valuemax={100} className="mt-2 h-2 w-full overflow-hidden rounded-full bg-muted">
      <div className={`h-full rounded-full transition-[width] ${p >= 90 ? "bg-destructive" : "bg-primary"}`} style={{ width: `${p}%` }} />
    </div>
  );
}

/** "a, b and c" */
function listWords(xs: string[]): string {
  if (xs.length <= 1) return xs.join("");
  return `${xs.slice(0, -1).join(", ")} and ${xs[xs.length - 1]}`;
}

/**
 * The database backend line. Today the server reports SQLite's journal mode and schema version; when
 * /api/system/info gains `database.backend` and `database.features` (ADR-0029 section 1.1, FB-155), they show here.
 */
export function BackendInfo({ info }: { info: SystemInfo }) {
  const db = info.database as SystemInfo["database"] & { backend?: string; features?: Record<string, { engine?: string; state?: string }> };
  const features = Object.entries(db.features ?? {});
  return (
    <span data-slot="backend-info" className="font-mono text-xs">
      {db.backend ? `${db.backend} · ` : ""}
      {db.journal_mode} · schema {db.schema_version}
      {features.length > 0 && ` · ${features.map(([k, v]) => `${k} ${v.engine ?? v.state ?? ""}`.trim()).join(", ")}`}
    </span>
  );
}

export function ServerHealth() {
  const headingId = useId();
  const prev = useRef<Reading>(undefined);
  const lastPresent = useRef<Set<MetricKey>>(new Set());
  const history = useRef<Point[]>([]);
  // An administrator has no entitlements of their own (the server answers 400 subject_required): ask for none.
  // While impersonating, the caller is the user, so ask for theirs.
  const session = useOptionalSession();
  const quotaSubject = useRef<"caller" | null>("caller");
  quotaSubject.current = session?.admin && !session.impersonation ? null : "caller";

  const { data, loading, updatedAt, refresh } = usePolling<Snapshot>(async (signal) => {
    const [reading, info, storage, vector, jobs, kv] = await Promise.all([
      part(async () => readingFrom(await getMetrics(signal), Date.now())),
      part(() => getSystemInfo(signal)),
      part(async () => {
        const [stats, quota] = await Promise.all([storageStats(signal), storageQuotaFor(quotaSubject.current, signal).catch(() => null)]);
        return { stats, quota };
      }),
      part(() => vectorStats(signal)),
      part(() => jobStats(signal)),
      part(() => kvInfo(signal)),
    ]);
    const present = presentIn(reading.state === "ok" ? reading.value : undefined);
    const reported = new Set([...present, ...lastPresent.current]);
    lastPresent.current = present;
    if (reading.state === "ok") {
      history.current = pushHistory(history.current, pointFrom(prev.current, reading.value), HISTORY);
      prev.current = reading.value;
    }
    return { reading, reported, info, storage, vector, jobs, kv, history: history.current };
  }, OVERVIEW_REFRESH_MS);

  const header = (
    <div className="flex flex-wrap items-end justify-between gap-3">
      <div>
        <h2 id={headingId} className="text-lg font-semibold tracking-tight">Server health</h2>
        <p className="mt-0.5 text-sm text-muted-foreground">
          Live figures from this server. Rates and latency need two readings, so they appear after a few seconds.
        </p>
      </div>
      <div className="flex items-center gap-3 text-xs text-muted-foreground">
        <span aria-live="polite">{updatedAt ? `Updated ${updatedAt.toLocaleTimeString()}` : "Not yet updated"} · every {OVERVIEW_REFRESH_MS / 1000} s</span>
        <Button onClick={refresh}>
          <HugeiconsIcon icon={RefreshIcon} size={16} strokeWidth={1.8} aria-hidden="true" />
          Refresh
        </Button>
      </div>
    </div>
  );

  if (loading && !data) {
    return (
      <section aria-labelledby={headingId} className="space-y-4">
        {header}
        <SkeletonCards count={6} label="Loading server health" />
      </section>
    );
  }
  if (!data) return null;

  const r = data.reading.state === "ok" ? data.reading.value : undefined;
  const has = (k: MetricKey) => data.reported.has(k);
  const hist = data.history;
  const last = hist[hist.length - 1];
  const kv = data.kv.state === "ok" ? data.kv.value : undefined;
  const jobTotals = data.jobs.state === "ok" ? data.jobs.value.totals : undefined;
  const jobCount = (k: string) => (jobTotals ? String(jobTotals[k] ?? 0) : undefined);
  const info = data.info.state === "ok" ? data.info.value : undefined;
  const dbSize = r?.dbSize ?? info?.database.size_bytes;
  const storage = data.storage.state === "ok" ? data.storage.value : undefined;
  const vec = data.vector.state === "ok" ? data.vector.value : undefined;

  // What the server does not report, in words, for the one muted line under the grid.
  const missing: string[] = [];
  const metricsOk = data.reading.state === "ok";
  const showCard = (keys: MetricKey[], name: string) => {
    if (!metricsOk) return false;
    if (keys.some(has)) return true;
    missing.push(name);
    return false;
  };
  const showPart = (p: Part<unknown>, name: string) => {
    if (p.state === "absent") {
      missing.push(name);
      return false;
    }
    return true;
  };

  const rpsCard = showCard(["requests"], "requests per second");
  const errCard = showCard(["errors"], "the error rate");
  const latCard = showCard(["latency"], "request latency");
  const connCard = showCard(["inFlight", "realtime"], "connections");
  if (connCard && has("inFlight") && !has("realtime")) missing.push("realtime connections");
  if (connCard && !has("inFlight") && has("realtime")) missing.push("active requests");
  const memCard = showCard(["rssBytes", "cpuSeconds"], "memory and CPU");
  if (memCard && has("rssBytes") && !has("cpuSeconds")) missing.push("CPU");
  if (memCard && !has("rssBytes") && has("cpuSeconds")) missing.push("memory");
  const storageCard = showPart(data.storage, "storage usage");
  const vecCard = showPart(data.vector, "vector indexes");
  const jobsCard = showPart(data.jobs, "jobs");
  const kvCard = showPart(data.kv, "the key-value store");

  return (
    <section aria-labelledby={headingId} className="space-y-4">
      {header}

      <div aria-label="Server" role="group" className="flex flex-wrap items-center gap-x-3 gap-y-1 text-sm">
        {info ? (
          <>
            <Badge variant="success" dot="pulse" size="sm">Online</Badge>
            <span>
              ForgeBase <span className="font-mono">{info.version}</span>
            </span>
            <span className="text-muted-foreground">· {info.mode} · up {formatUptime(info.uptime_secs)}</span>
          </>
        ) : data.info.state === "absent" ? (
          <span className="text-muted-foreground">This server does not report its version.</span>
        ) : (
          <Problem p={data.info} />
        )}
      </div>

      {data.reading.state === "error" && <ErrorPanel error={data.reading.error} onRetry={refresh} />}
      {data.reading.state === "denied" && <p className="text-sm text-warning">Your account cannot read the server metrics (/metrics).</p>}

      <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
        <Card title="Database" icon={Database01Icon}>
          <Stat label="Size" value={dbSize === undefined ? undefined : formatBytes(dbSize)} sub={r?.walSize !== undefined ? `WAL (write-ahead log) ${formatBytes(r.walSize)}` : undefined} />
          <div className="mt-2 flex flex-wrap items-center gap-2 text-sm text-foreground/80">
            {info ? <BackendInfo info={info} /> : <Problem p={data.info} />}
          </div>
        </Card>

        {rpsCard && (
          <Card title="Requests per second" icon={Activity01Icon}>
            <Stat label="Now" value={num(last?.rps, 2)} sub={r?.requests !== undefined ? `${r.requests} total` : undefined} />
            <Sparkline label="Requests per second history" values={hist.map((p) => p.rps)} />
          </Card>
        )}

        {errCard && (
          <Card title="Error rate" icon={Alert02Icon}>
            <Stat label="Server errors (5xx), last interval" value={pct(last?.errorRate === undefined ? undefined : last.errorRate * 100)} sub={r?.errors !== undefined ? `${r.errors} server errors total` : undefined} />
            <Sparkline label="Error rate history" values={hist.map((p) => (p.errorRate === undefined ? undefined : p.errorRate * 100))} />
          </Card>
        )}

        {latCard && (
          <Card title="Request latency" icon={Clock01Icon}>
            <div className="grid grid-cols-2 gap-3">
              <Stat label="Median (p50)" value={ms(last?.p50)} />
              <Stat label="Slowest 5% (p95)" value={ms(last?.p95)} />
            </div>
            <Sparkline label="p95 latency history" values={hist.map((p) => p.p95)} />
          </Card>
        )}

        {connCard && (
          <Card title="Connections" icon={Plug01Icon}>
            <div className="grid grid-cols-2 gap-3">
              {has("inFlight") && <Stat label="Active requests" value={r?.inFlight === undefined ? undefined : String(r.inFlight)} />}
              {has("realtime") && <Stat label="Realtime" value={r?.realtime === undefined ? undefined : String(r.realtime)} />}
            </div>
            {r?.resp !== undefined && <p className="mt-2 text-xs text-muted-foreground">{r.resp} Redis protocol (RESP) connections</p>}
            {has("realtime") ? (
              <Sparkline label="Realtime connections history" values={hist.map((p) => p.realtime)} />
            ) : (
              <Sparkline label="Active requests history" values={hist.map((p) => p.inFlight)} />
            )}
          </Card>
        )}

        {memCard && (
          <Card title="Memory and CPU" icon={CpuIcon}>
            <div className="grid grid-cols-2 gap-3">
              {has("rssBytes") && <Stat label="Memory in use" value={r?.rssBytes === undefined ? undefined : formatBytes(r.rssBytes)} />}
              {has("cpuSeconds") && <Stat label="CPU" value={pct(last?.cpuPct)} />}
            </div>
            {has("rssBytes") ? (
              <Sparkline label="Memory history" values={hist.map((p) => p.rssBytes)} />
            ) : (
              <Sparkline label="CPU history" values={hist.map((p) => p.cpuPct)} />
            )}
          </Card>
        )}

        {storageCard && (
          <Card title="Storage usage" icon={Folder02Icon}>
            {storage ? (
              <>
                <Stat label="Stored" value={formatBytes(storage.stats.stored_bytes)} sub={`${storage.stats.objects} files in ${storage.stats.buckets} buckets · ${storage.stats.open_uploads} uploads in progress`} />
                {storage.quota !== null ? (
                  <>
                    <p aria-label="Storage quota usage" className="mt-2 text-xs text-muted-foreground">
                      {formatBytes(storage.stats.logical_bytes)} of {formatBytes(storage.quota)} quota
                    </p>
                    <Meter label="Storage quota" used={storage.stats.logical_bytes} limit={storage.quota} />
                  </>
                ) : (
                  <p className="mt-2 text-xs text-muted-foreground">No storage limit is set.</p>
                )}
              </>
            ) : (
              <Problem p={data.storage} />
            )}
          </Card>
        )}

        {vecCard && (
          <Card title="Vector indexes" icon={AiNetworkIcon}>
            {vec ? (
              <div className="grid grid-cols-2 gap-3">
                <Stat label="Collections" value={String(vec.collections)} sub={`${vec.indexes} indexes`} />
                <Stat label="Indexed vectors" value={String(vec.indexed_vectors)} sub={`${formatBytes(vec.memory_bytes)} memory${vec.building ? ` · ${vec.building} building` : ""}`} />
              </div>
            ) : (
              <Problem p={data.vector} />
            )}
          </Card>
        )}

        {jobsCard && (
          <Card title="Jobs" icon={TaskDaily01Icon}>
            {jobTotals ? (
              <div className="grid grid-cols-2 gap-3 sm:grid-cols-4 sm:gap-2">
                <Stat label="Queued" value={jobCount("queued")} />
                <Stat label="Running" value={jobCount("running")} />
                <Stat label="Dead" value={jobCount("dead")} />
                <Stat label="Succeeded" value={jobCount("succeeded")} />
              </div>
            ) : (
              <Problem p={data.jobs} />
            )}
          </Card>
        )}

        {kvCard && (
          <Card title="Key-value store" icon={ZapIcon}>
            {kv ? (
              <div className="grid grid-cols-2 gap-3">
                <Stat label="Keys" value={String(kv.keys)} sub={`${kv.hits} hits · ${kv.misses} misses`} />
                <Stat label="Memory" value={formatBytes(kv.used_memory_bytes)} sub={kv.max_memory_bytes ? `of ${formatBytes(kv.max_memory_bytes)}` : "no limit"} />
              </div>
            ) : (
              <Problem p={data.kv} />
            )}
          </Card>
        )}
      </div>

      {missing.length > 0 && (
        <p className="text-xs text-muted-foreground">
          This server does not report {listWords(missing)}. <DocsLink doc="docs/deployment.md#51-metrics-metrics" label="About metrics" />
        </p>
      )}
    </section>
  );
}

export default ServerHealth;
