import { ApiError, getHealth, getReady, getSystemInfo } from "../api";
import type { HealthResponse, ReadyResponse, SystemInfo } from "../api";
import { formatBytes, formatUptime } from "../format";
import { usePolling } from "../hooks";

const REFRESH_MS = 5000;

interface Snapshot {
  health: PromiseSettledResult<HealthResponse>;
  ready: PromiseSettledResult<ReadyResponse>;
  info: PromiseSettledResult<SystemInfo>;
}

async function load(signal: AbortSignal): Promise<Snapshot> {
  const [health, ready, info] = await Promise.allSettled([getHealth(signal), getReady(signal), getSystemInfo(signal)]);
  return { health, ready, info };
}

function reason(r: PromiseRejectedResult): string {
  const e = r.reason;
  if (e instanceof ApiError) return `${e.code}: ${e.message}${e.requestId ? ` (request ${e.requestId})` : ""}`;
  return e instanceof Error ? e.message : String(e);
}

const isNetworkDown = (s: Snapshot) =>
  [s.health, s.ready, s.info].every((r) => r.status === "rejected" && r.reason instanceof ApiError && r.reason.isNetwork);

function Badge({ tone, children }: { tone: "ok" | "bad" | "muted"; children: React.ReactNode }) {
  const c = tone === "ok" ? "bg-emerald-500/15 text-emerald-300" : tone === "bad" ? "bg-red-500/15 text-red-300" : "bg-zinc-700/40 text-zinc-300";
  return <span className={`rounded px-2 py-0.5 text-xs font-medium ${c}`}>{children}</span>;
}

function Card({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <section className="rounded-lg border border-zinc-800 bg-zinc-900/60 p-4">
      <h2 className="mb-3 text-sm font-medium text-zinc-400">{title}</h2>
      {children}
    </section>
  );
}

function Row({ label, value }: { label: string; value: React.ReactNode }) {
  return (
    <div className="flex justify-between gap-4 py-1 text-sm">
      <dt className="text-zinc-400">{label}</dt>
      <dd className="min-w-0 break-all text-right font-mono text-zinc-100">{value}</dd>
    </div>
  );
}

function Failed({ r }: { r: PromiseRejectedResult }) {
  return (
    <p role="alert" className="text-sm text-red-300">
      {reason(r)}
    </p>
  );
}

export default function SystemPage() {
  const { data, loading, updatedAt, refresh } = usePolling(load, REFRESH_MS);

  return (
    <div className="max-w-4xl">
      <div className="flex items-start justify-between gap-4">
        <div>
          <h1 className="text-2xl font-semibold tracking-tight">System</h1>
          <p className="mt-1 text-zinc-400">Live server health and build information.</p>
        </div>
        <div className="text-right text-xs text-zinc-500">
          <button type="button" onClick={refresh} className="rounded-md border border-zinc-700 px-3 py-1 text-sm text-zinc-200 hover:bg-zinc-800">
            Refresh
          </button>
          <p className="mt-1">
            {updatedAt ? `Updated ${updatedAt.toLocaleTimeString()}` : "Not yet updated"} · every {REFRESH_MS / 1000}s
          </p>
        </div>
      </div>

      {loading && !data && <p className="mt-6 text-zinc-400">Loading…</p>}

      {data && isNetworkDown(data) && (
        <div role="alert" className="mt-6 rounded-lg border border-red-500/40 bg-red-500/10 p-4">
          <p className="font-medium text-red-300">Server unreachable</p>
          <p className="mt-1 text-sm text-red-200/80">
            Could not reach the ForgeBase server. Retrying every {REFRESH_MS / 1000}s. In development, start it on 127.0.0.1:8090.
          </p>
        </div>
      )}

      {data && !isNetworkDown(data) && (
        <div className="mt-6 grid gap-4 md:grid-cols-2">
          <Card title="Status">
            <dl>
              <Row
                label="Liveness (/health)"
                value={
                  data.health.status === "fulfilled" ? (
                    <Badge tone={data.health.value.status === "ok" ? "ok" : "bad"}>{data.health.value.status}</Badge>
                  ) : (
                    <Badge tone="bad">error</Badge>
                  )
                }
              />
              <Row
                label="Readiness (/health/ready)"
                value={
                  data.ready.status === "fulfilled" ? (
                    <Badge tone={data.ready.value.status === "ready" ? "ok" : "bad"}>{data.ready.value.status}</Badge>
                  ) : (
                    <Badge tone="bad">error</Badge>
                  )
                }
              />
            </dl>
            {data.health.status === "rejected" && <Failed r={data.health} />}
            {data.ready.status === "rejected" && <Failed r={data.ready} />}
            {data.ready.status === "fulfilled" && (
              <ul className="mt-2 space-y-1" aria-label="Readiness checks">
                {Object.entries(data.ready.value.checks).map(([name, st]) => (
                  <li key={name} className="flex justify-between text-sm">
                    <span className="text-zinc-400">{name}</span>
                    <Badge tone={st === "ok" ? "ok" : "bad"}>{st}</Badge>
                  </li>
                ))}
              </ul>
            )}
          </Card>

          {data.info.status === "fulfilled" ? (
            <>
              <Card title="Server">
                <dl>
                  <Row label="Name" value={data.info.value.name} />
                  <Row label="Version" value={data.info.value.version} />
                  <Row label="Mode" value={data.info.value.mode} />
                  <Row label="Uptime" value={formatUptime(data.info.value.uptime_secs)} />
                  <Row label="Started" value={data.info.value.started_at} />
                  <Row label="Data dir" value={data.info.value.data_dir} />
                </dl>
              </Card>
              <Card title="Database">
                <dl>
                  <Row label="Path" value={data.info.value.database.path} />
                  <Row label="Size" value={formatBytes(data.info.value.database.size_bytes)} />
                  <Row label="Journal mode" value={data.info.value.database.journal_mode} />
                  <Row label="Schema version" value={data.info.value.database.schema_version} />
                </dl>
              </Card>
            </>
          ) : (
            <Card title="Server">
              <Failed r={data.info} />
            </Card>
          )}

          <Card title="Coming later">
            <p className="text-sm text-zinc-400">
              Server and error logs, audit logs, migrations, backups, restore, configuration and system metrics arrive in phase 9.
            </p>
          </Card>
        </div>
      )}
    </div>
  );
}
