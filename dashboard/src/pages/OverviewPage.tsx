import { useRef } from "react";
import { ApiError, getMetrics, getSystemInfo, jobStats, kvInfo, pointFrom, pushHistory, readingFrom, storageQuotaBytes, storageStats, vectorStats } from "../api";
import type { JobStats, KvStats, Point, Reading, StorageStats, SystemInfo, VectorStats } from "../api";
import { formatBytes, formatUptime } from "../format";
import { usePolling } from "../hooks";

export const OVERVIEW_REFRESH_MS = 5000;
const HISTORY = 60;
const NOT_REPORTED = "not reported by this server";

/** One source of a card: a value, an absent answer (404/501 or no such metric) or a failure. */
type Part<T> = { state: "ok"; value: T } | { state: "absent" } | { state: "denied" } | { state: "error"; message: string };

async function part<T>(f: () => Promise<T>): Promise<Part<T>> {
  try {
    return { state: "ok", value: await f() };
  } catch (e) {
    if (e instanceof ApiError) {
      if (e.status === 404 || e.status === 501) return { state: "absent" };
      if (e.isAuth) return { state: "denied" };
      return { state: "error", message: `${e.code}: ${e.message}` };
    }
    if (e instanceof DOMException && e.name === "AbortError") throw e;
    return { state: "error", message: e instanceof Error ? e.message : String(e) };
  }
}

interface Snapshot {
  reading: Part<Reading>;
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

function Card({ title, children, label }: { title: string; children: React.ReactNode; label?: string }) {
  return (
    <section aria-label={label ?? title} className="rounded-lg border border-zinc-800 bg-zinc-900/60 p-4">
      <h2 className="mb-2 text-sm font-medium text-zinc-400">{title}</h2>
      {children}
    </section>
  );
}

function Missing({ p }: { p?: Part<unknown> }) {
  if (p?.state === "denied") return <p className="text-sm text-amber-300">Sign-in or permission required</p>;
  if (p?.state === "error") return <p role="alert" className="text-sm text-red-300">{p.message}</p>;
  return <p className="text-sm text-zinc-500">{NOT_REPORTED}</p>;
}

/** A big figure, or the not-reported line when the value is absent. */
function Stat({ label, value, sub, absent }: { label: string; value: string | undefined; sub?: string; absent?: Part<unknown> }) {
  return (
    <div>
      <p className="text-xs text-zinc-500">{label}</p>
      {value === undefined ? <Missing p={absent} /> : <p className="font-mono text-2xl text-zinc-100">{value}</p>}
      {value !== undefined && sub && <p className="text-xs text-zinc-500">{sub}</p>}
    </div>
  );
}

/** Tiny inline-SVG time series; points with an undefined value leave gaps. */
export function Sparkline({ label, values }: { label: string; values: (number | undefined)[] }) {
  const defined = values.filter((v): v is number => v !== undefined);
  if (defined.length < 2) return <p className="mt-2 text-xs text-zinc-600">collecting…</p>;
  const w = 200;
  const h = 40;
  const max = Math.max(...defined, 1e-9);
  const min = Math.min(...defined, 0);
  const span = max - min || 1;
  const step = w / Math.max(values.length - 1, 1);
  const segs: string[] = [];
  let cur = "";
  values.forEach((v, i) => {
    if (v === undefined) {
      if (cur) segs.push(cur);
      cur = "";
      return;
    }
    cur += `${cur ? "L" : "M"}${(i * step).toFixed(1)},${(h - ((v - min) / span) * (h - 2) - 1).toFixed(1)}`;
  });
  if (cur) segs.push(cur);
  return (
    <svg role="img" aria-label={label} viewBox={`0 0 ${w} ${h}`} className="mt-2 h-10 w-full text-amber-400" preserveAspectRatio="none">
      {segs.map((d, i) => (
        <path key={i} d={d} fill="none" stroke="currentColor" strokeWidth="1.5" vectorEffect="non-scaling-stroke" />
      ))}
    </svg>
  );
}

function Meter({ label, used, limit }: { label: string; used: number; limit: number }) {
  const p = limit > 0 ? Math.min(100, Math.round((used / limit) * 100)) : 0;
  return (
    <div role="progressbar" aria-label={label} aria-valuenow={p} aria-valuemin={0} aria-valuemax={100} className="mt-2 h-2 w-full overflow-hidden rounded bg-zinc-800">
      <div className={`h-full ${p >= 90 ? "bg-red-500" : "bg-amber-500"}`} style={{ width: `${p}%` }} />
    </div>
  );
}

export default function OverviewPage() {
  const prev = useRef<Reading>(undefined);
  const history = useRef<Point[]>([]);

  const { data, loading, updatedAt, refresh } = usePolling<Snapshot>(async (signal) => {
    const [reading, info, storage, vector, jobs, kv] = await Promise.all([
      part(async () => readingFrom(await getMetrics(signal), Date.now())),
      part(() => getSystemInfo(signal)),
      part(async () => {
        const [stats, quota] = await Promise.all([storageStats(signal), storageQuotaBytes(signal).catch(() => null)]);
        return { stats, quota };
      }),
      part(() => vectorStats(signal)),
      part(() => jobStats(signal)),
      part(() => kvInfo(signal)),
    ]);
    if (reading.state === "ok") {
      history.current = pushHistory(history.current, pointFrom(prev.current, reading.value), HISTORY);
      prev.current = reading.value;
    }
    return { reading, info, storage, vector, jobs, kv, history: history.current };
  }, OVERVIEW_REFRESH_MS);

  const r = data?.reading.state === "ok" ? data.reading.value : undefined;
  const hist = data?.history ?? [];
  const last = hist[hist.length - 1];
  const metricsDown = data !== undefined && data.reading.state !== "ok";
  const kv = data?.kv.state === "ok" ? data.kv.value : undefined;
  const jobTotals = data?.jobs.state === "ok" ? data.jobs.value.totals : undefined;
  const jobCount = (k: string) => (jobTotals ? String(jobTotals[k] ?? 0) : undefined);
  const dbSize = r?.dbSize ?? (data?.info.state === "ok" ? data.info.value.database.size_bytes : undefined);
  const storage = data?.storage.state === "ok" ? data.storage.value : undefined;
  const vec = data?.vector.state === "ok" ? data.vector.value : undefined;

  return (
    <div className="max-w-6xl">
      <div className="flex items-start justify-between gap-4">
        <div>
          <h1 className="text-2xl font-semibold tracking-tight">Dashboard</h1>
          <p className="mt-1 text-zinc-400">Live metrics overview. Figures that need two scrapes (rates, latency of the last interval) appear after the second poll.</p>
        </div>
        <div className="text-right text-xs text-zinc-500">
          <button type="button" onClick={refresh} className="rounded-md border border-zinc-700 px-3 py-1 text-sm text-zinc-200 hover:bg-zinc-800">
            Refresh
          </button>
          <p className="mt-1">
            {updatedAt ? `Updated ${updatedAt.toLocaleTimeString()}` : "Not yet updated"} · every {OVERVIEW_REFRESH_MS / 1000}s
          </p>
        </div>
      </div>

      {loading && !data && <p className="mt-6 text-zinc-400">Loading…</p>}
      {metricsDown && data.reading.state === "error" && (
        <p role="alert" className="mt-4 rounded border border-red-800/60 bg-red-500/10 p-3 text-sm text-red-300">
          Could not read /metrics: {data.reading.message}
        </p>
      )}

      {data && (
        <div className="mt-6 grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
          <Card title="Database">
            <Stat label="Size" value={dbSize === undefined ? undefined : formatBytes(dbSize)} sub={r?.walSize !== undefined ? `WAL ${formatBytes(r.walSize)}` : undefined} absent={data.info} />
            <div className="mt-2 text-sm text-zinc-300">
              {data.info.state === "ok" ? (
                <>
                  <span className="rounded bg-emerald-500/15 px-2 py-0.5 text-xs text-emerald-300">online</span>{" "}
                  <span className="font-mono text-xs">{data.info.value.database.journal_mode} · schema {data.info.value.database.schema_version} · up {formatUptime(data.info.value.uptime_secs)}</span>
                </>
              ) : (
                <Missing p={data.info} />
              )}
            </div>
          </Card>

          <Card title="Requests per second">
            <Stat label="Now" value={num(last?.rps, 2)} sub={r?.requests !== undefined ? `${r.requests} total` : undefined} absent={data.reading} />
            <Sparkline label="Requests per second history" values={hist.map((p) => p.rps)} />
          </Card>

          <Card title="Error rate">
            <Stat label="5xx share, last interval" value={pct(last?.errorRate === undefined ? undefined : last.errorRate * 100)} sub={r?.errors !== undefined ? `${r.errors} server errors total` : undefined} absent={data.reading} />
            <Sparkline label="Error rate history" values={hist.map((p) => (p.errorRate === undefined ? undefined : p.errorRate * 100))} />
          </Card>

          <Card title="Query latency">
            <div className="grid grid-cols-2 gap-3">
              <Stat label="p50" value={ms(last?.p50)} absent={data.reading} />
              <Stat label="p95" value={ms(last?.p95)} absent={data.reading} />
            </div>
            <Sparkline label="p95 latency history" values={hist.map((p) => p.p95)} />
          </Card>

          <Card title="Connections">
            <div className="grid grid-cols-2 gap-3">
              <Stat label="Active requests" value={r?.inFlight === undefined ? undefined : String(r.inFlight)} absent={data.reading} />
              <Stat label="Realtime" value={r?.realtime === undefined ? undefined : String(r.realtime)} absent={data.reading} />
            </div>
            {r?.resp !== undefined && <p className="mt-2 text-xs text-zinc-500">{r.resp} Redis (RESP) connections</p>}
            <Sparkline label="Realtime connections history" values={hist.map((p) => p.realtime)} />
          </Card>

          <Card title="Memory and CPU">
            <div className="grid grid-cols-2 gap-3">
              <Stat label="Memory (RSS)" value={r?.rssBytes === undefined ? undefined : formatBytes(r.rssBytes)} absent={data.reading} />
              <Stat label="CPU" value={pct(last?.cpuPct)} absent={data.reading} />
            </div>
            <Sparkline label="Memory history" values={hist.map((p) => p.rssBytes)} />
          </Card>

          <Card title="Storage usage">
            {storage ? (
              <>
                <Stat label="Stored" value={formatBytes(storage.stats.stored_bytes)} sub={`${storage.stats.objects} objects in ${storage.stats.buckets} buckets · ${storage.stats.open_uploads} open uploads`} />
                {storage.quota !== null ? (
                  <>
                    <p aria-label="Storage quota usage" className="mt-2 text-xs text-zinc-400">
                      {formatBytes(storage.stats.logical_bytes)} of {formatBytes(storage.quota)} quota
                    </p>
                    <Meter label="Storage quota" used={storage.stats.logical_bytes} limit={storage.quota} />
                  </>
                ) : (
                  <p className="mt-2 text-xs text-zinc-500">No quota reported for this account</p>
                )}
              </>
            ) : (
              <Missing p={data.storage} />
            )}
          </Card>

          <Card title="Vector indexes">
            {vec ? (
              <div className="grid grid-cols-2 gap-3">
                <Stat label="Collections" value={String(vec.collections)} sub={`${vec.indexes} indexes`} />
                <Stat label="Indexed vectors" value={String(vec.indexed_vectors)} sub={`${formatBytes(vec.memory_bytes)} memory${vec.building ? ` · ${vec.building} building` : ""}`} />
              </div>
            ) : (
              <Missing p={data.vector} />
            )}
          </Card>

          <Card title="Jobs">
            {jobTotals ? (
              <div className="grid grid-cols-4 gap-2">
                <Stat label="Queued" value={jobCount("queued")} />
                <Stat label="Running" value={jobCount("running")} />
                <Stat label="Dead" value={jobCount("dead")} />
                <Stat label="Succeeded" value={jobCount("succeeded")} />
              </div>
            ) : (
              <Missing p={data.jobs} />
            )}
          </Card>

          <Card title="Key-value store">
            {kv ? (
              <div className="grid grid-cols-2 gap-3">
                <Stat label="Keys" value={String(kv.keys)} sub={`${kv.hits} hits · ${kv.misses} misses`} />
                <Stat label="Memory" value={formatBytes(kv.used_memory_bytes)} sub={kv.max_memory_bytes ? `of ${formatBytes(kv.max_memory_bytes)}` : "no limit"} />
              </div>
            ) : (
              <Missing p={data.kv} />
            )}
          </Card>
        </div>
      )}
    </div>
  );
}
