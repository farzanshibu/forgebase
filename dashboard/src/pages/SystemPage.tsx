import { ApiError, getHealth, getReady, getSystemInfo } from "../api";
import type { HealthResponse, ReadyResponse, SystemInfo } from "../api";
import { formatBytes, formatUptime } from "../format";
import { usePolling } from "../hooks";
import { Badge as PanelBadge } from "panelui-web/badge";
import { Card as PanelCard } from "panelui-web/card";
import { Link } from "react-router-dom";
import { PageHeader } from "../components/PageHeader";
import { SkeletonCards } from "../components/Skeleton";
import { Button } from "../components/ui";
import { BACKEND_NAME, SQLITE_NOTICE, showSqliteNotice, type DatabaseFeature, type DatabaseInfo } from "./database/info";

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
  return (
    <PanelBadge variant={tone === "ok" ? "success" : tone === "bad" ? "destructive" : "secondary"} dot={tone !== "muted"}>
      {children}
    </PanelBadge>
  );
}

function Card({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <PanelCard as="section" size="sm" className="rounded-xl">
      <PanelCard.Header className="pb-3">
        <PanelCard.Title as="h2" className="text-sm font-medium text-muted-foreground">
          {title}
        </PanelCard.Title>
      </PanelCard.Header>
      <PanelCard.Content>{children}</PanelCard.Content>
    </PanelCard>
  );
}

function Row({ label, value }: { label: string; value: React.ReactNode }) {
  return (
    <div className="flex justify-between gap-4 py-1 text-sm">
      <dt className="text-muted-foreground">{label}</dt>
      <dd className="min-w-0 break-all text-right font-mono text-foreground">{value}</dd>
    </div>
  );
}

function Failed({ r }: { r: PromiseRejectedResult }) {
  return (
    <p role="alert" className="text-sm text-destructive">
      {reason(r)}
    </p>
  );
}

const STATE_WORDS: Record<DatabaseFeature["state"], { label: string; variant: "success" | "secondary" | "outline"; text: string }> = {
  active: { label: "active", variant: "success", text: "Runs on the PostgreSQL extension." },
  portable: { label: "portable", variant: "secondary", text: "Runs on ForgeBase's built-in version, which works without the extension." },
  off: { label: "off", variant: "outline", text: "Switched off." },
};

const FEATURE_NAMES: Record<string, string> = { vector: "Vector search", graph: "Graph", queue: "Queues", cron: "Schedules", trigram: "Fuzzy text search" };

/** The database backend and, on PostgreSQL, whether each extension feature runs on the extension or on the portable fallback (FB-148, FB-161). */
function DatabaseCard({ info }: { info: SystemInfo }) {
  const db = info.database as SystemInfo["database"] & Partial<DatabaseInfo>;
  const features = Object.entries(db.features ?? {});
  return (
    <>
      <Card title="Database">
        <dl>
          {db.backend && <Row label="Backend" value={BACKEND_NAME[db.backend] ?? db.backend} />}
          {db.mode && <Row label="Mode" value={db.mode} />}
          {db.server_version && <Row label="Server version" value={db.server_version} />}
          {typeof db.connections === "number" && <Row label="Connections" value={db.connections} />}
          {db.path && <Row label="Path" value={db.path} />}
          {typeof db.size_bytes === "number" && <Row label="Size" value={formatBytes(db.size_bytes)} />}
          {db.journal_mode && <Row label="Journal mode" value={db.journal_mode} />}
          <Row label="Schema version" value={db.schema_version} />
        </dl>
        {showSqliteNotice(db) && (
          <p role="status" data-testid="sqlite-notice" className="mt-3 rounded-md border border-warning/40 bg-warning/10 p-2 text-sm">
            {SQLITE_NOTICE.split("`")[0]}<code>{SQLITE_NOTICE.split("`")[1]}</code>{SQLITE_NOTICE.split("`")[2]}
          </p>
        )}
      </Card>
      {features.length > 0 && (
        <Card title="Extension features">
          <ul aria-label="Extension features" className="space-y-2">
            {features.map(([id, f]) => {
              const w = STATE_WORDS[f.state] ?? STATE_WORDS.off;
              return (
                <li key={id} className="text-sm" data-feature={id}>
                  <div className="flex items-center justify-between gap-2">
                    <span>{FEATURE_NAMES[id] ?? id}</span>
                    <PanelBadge variant={w.variant} size="sm" title={w.text}>{w.label}</PanelBadge>
                  </div>
                  <p className="text-xs text-muted-foreground">{[f.engine && f.engine !== "portable" ? f.engine : undefined, f.reason ?? w.text].filter(Boolean).join(" · ")}</p>
                </li>
              );
            })}
          </ul>
          <p className="mt-3 text-xs text-muted-foreground">Active runs on a PostgreSQL extension; portable runs on ForgeBase&apos;s built-in version; off is switched off.</p>
        </Card>
      )}
    </>
  );
}

export default function SystemPage() {
  const { data, loading, updatedAt, refresh } = usePolling(load, REFRESH_MS);

  return (
    <div className="max-w-4xl">
      <PageHeader
        studio="system"
        description="Live server health and build information, and where to find backups, the audit log and the server's settings."
        secondary={<Button onClick={refresh}>Refresh</Button>}
      >
        <p className="text-xs text-muted-foreground">
          {updatedAt ? `Updated ${updatedAt.toLocaleTimeString()}` : "Not yet updated"} · every {REFRESH_MS / 1000}s
        </p>
      </PageHeader>

      {loading && !data && <div className="mt-6"><SkeletonCards count={4} label="Loading server health" /></div>}

      {data && isNetworkDown(data) && (
        <div role="alert" className="mt-6 rounded-lg border border-destructive/40 bg-destructive/10 p-4">
          <p className="font-medium text-destructive">Server unreachable</p>
          <p className="mt-1 text-sm text-destructive">
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
                    <span className="text-muted-foreground">{name}</span>
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
              <DatabaseCard info={data.info.value} />
            </>
          ) : (
            <Card title="Server">
              <Failed r={data.info} />
            </Card>
          )}

          <Card title="More about this server">
            <ul className="space-y-1.5 text-sm">
              <li><Link className="text-primary underline-offset-2 hover:underline" to="/system/backups">Backups</Link> <span className="text-muted-foreground">: take, verify and prune snapshots.</span></li>
              <li><Link className="text-primary underline-offset-2 hover:underline" to="/auth/audit">Audit log</Link> <span className="text-muted-foreground">: who signed in and who changed what.</span></li>
              <li><Link className="text-primary underline-offset-2 hover:underline" to="/system/features">Features and settings</Link> <span className="text-muted-foreground">: switch features on or off and edit the server&apos;s settings.</span></li>
            </ul>
          </Card>
        </div>
      )}
    </div>
  );
}
