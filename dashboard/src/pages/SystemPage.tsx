import { ApiError, getHealth, getReady, getSystemInfo } from "../api";
import type { HealthResponse, ReadyResponse, SystemInfo } from "../api";
import { formatBytes, formatUptime } from "../format";
import { usePolling } from "../hooks";
import { Badge as PanelBadge } from "panelui-web/badge";
import { Link } from "react-router-dom";
import { ArrowRight01Icon, Database01Icon, Link01Icon, Pulse01Icon, PuzzleIcon, ServerStack01Icon } from "@hugeicons/core-free-icons";
import { HugeiconsIcon } from "@hugeicons/react";
import { CopyIcon, Dot, When, type Tone } from "./jobs/ops";
import { PageHeader } from "../components/PageHeader";
import { SkeletonCards } from "../components/Skeleton";
import { Button } from "../components/ui";
import { Card, Row } from "./system/cards";
import TlsCard from "./system/TlsCard";
import TracingCard from "./system/TracingCard";
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

/** A file path: on its own line under the label, cut to the width with the whole path on hover, and a copy button. */
function PathRow({ label, path }: { label: string; path: string }) {
  return (
    <div className="border-b border-border/60 py-2 text-sm last:border-b-0">
      <dt className="text-muted-foreground">{label}</dt>
      <dd className="mt-1 flex min-w-0 items-center gap-1 rounded-md bg-muted/50 py-0.5 pl-2 pr-0.5">
        <span className="min-w-0 flex-1 truncate font-mono text-xs [direction:rtl] text-left" title={path}><bdi>{path}</bdi></span>
        <CopyIcon text={path} label={`Copy ${label.toLowerCase()}`} />
      </dd>
    </div>
  );
}

type Health = { tone: Exclude<Tone, "default">; title: string; text: string };

/** One verdict from liveness and readiness: Healthy, Not ready, or Down. */
function verdict(s: Snapshot): Health {
  const live = s.health.status === "fulfilled" && s.health.value.status === "ok";
  if (!live) return { tone: "bad", title: "Down", text: "The liveness check fails: the server is not answering as it should. Check its log." };
  if (s.ready.status !== "fulfilled") return { tone: "warn", title: "Not ready", text: "The readiness check could not be read. Requests may fail until it passes." };
  const failing = Object.entries(s.ready.value.checks).filter(([, st]) => st !== "ok").map(([n]) => n);
  if (s.ready.value.status !== "ready" || failing.length > 0) {
    return { tone: "warn", title: "Not ready", text: failing.length ? `The server is up, but these checks fail: ${failing.join(", ")}. Requests that need them fail until they pass.` : "The server is up but not ready to serve requests yet." };
  }
  return { tone: "ok", title: "Healthy", text: "The server is up and ready to serve requests. Every readiness check passes." };
}

function HealthBanner({ snap, updatedAt }: { snap: Snapshot; updatedAt?: Date }) {
  const v = verdict(snap);
  const checks = snap.ready.status === "fulfilled" ? Object.values(snap.ready.value.checks) : [];
  const passing = checks.filter((c) => c === "ok").length;
  const box = { ok: "border-success/40 bg-success/5", warn: "border-warning/50 bg-warning/10", bad: "border-destructive/50 bg-destructive/10", info: "border-border", neutral: "border-border" }[v.tone];
  const word = { ok: "text-success", warn: "text-warning", bad: "text-destructive", info: "", neutral: "" }[v.tone];
  return (
    <section aria-label="Overall health" className={`flex flex-wrap items-center gap-x-6 gap-y-2 rounded-xl border p-4 ${box}`}>
      <div className="flex min-w-0 flex-1 items-start gap-3">
        <span className="mt-1.5"><Dot tone={v.tone} pulse={v.tone !== "ok"} /></span>
        <div className="min-w-0">
          <p className={`text-base font-semibold ${word}`}>{v.title}</p>
          <p className="text-sm text-muted-foreground">{v.text}</p>
        </div>
      </div>
      <div className="text-right text-xs text-muted-foreground">
        {checks.length > 0 && <p className="tabular-nums">{passing} of {checks.length} checks passing</p>}
        {updatedAt && <p className="tabular-nums">Checked {updatedAt.toLocaleTimeString()}</p>}
      </div>
    </section>
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
      <Card title="Database" icon={Database01Icon}>
        <dl>
          {db.backend && <Row label="Backend" value={BACKEND_NAME[db.backend] ?? db.backend} />}
          {db.mode && <Row label="Mode" value={db.mode} />}
          {db.server_version && <Row label="Server version" value={db.server_version} mono />}
          {typeof db.connections === "number" && <Row label="Connections" value={db.connections} />}
          {db.path && <PathRow label="Path" path={db.path} />}
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
        <Card title="Extension features" icon={PuzzleIcon}>
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
  const down = data ? isNetworkDown(data) : false;

  return (
    <div className="max-w-5xl space-y-6">
      <PageHeader
        studio="system"
        description="Live server health and build information, and where to find backups, the audit log and the server's settings."
        secondary={<Button onClick={refresh}>Refresh</Button>}
      >
        <p className="mt-1 flex items-center gap-1.5 text-xs text-muted-foreground">
          <Dot tone={down ? "bad" : updatedAt ? "ok" : "neutral"} pulse={!down && !!updatedAt} />
          {updatedAt ? `Live · updated ${updatedAt.toLocaleTimeString()}` : "Not yet updated"} · every {REFRESH_MS / 1000}s
        </p>
      </PageHeader>

      {loading && !data && <SkeletonCards count={4} label="Loading server health" />}

      {data && down && (
        <div role="alert" className="rounded-xl border border-destructive/40 bg-destructive/10 p-4">
          <p className="flex items-center gap-2 font-medium text-destructive"><Dot tone="bad" pulse />Server unreachable</p>
          <p className="mt-1 text-sm text-destructive">
            Could not reach the ForgeBase server. Retrying every {REFRESH_MS / 1000}s. In development, start it on 127.0.0.1:8090.
          </p>
        </div>
      )}

      {data && !down && (
        <>
          <HealthBanner snap={data} updatedAt={updatedAt} />
          <div className="grid gap-4 md:grid-cols-2">
            <Card title="Status" icon={Pulse01Icon}>
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
              {data.ready.status === "fulfilled" && Object.keys(data.ready.value.checks).length > 0 && (
                <>
                  <h3 className="mb-1 mt-4 text-xs font-medium uppercase tracking-wide text-muted-foreground">Readiness checks</h3>
                  <ul className="divide-y divide-border/60 rounded-lg border border-border" aria-label="Readiness checks">
                    {Object.entries(data.ready.value.checks).map(([name, st]) => (
                      <li key={name} className="flex items-center justify-between px-3 py-1.5 text-sm">
                        <span className="capitalize">{name}</span>
                        <Badge tone={st === "ok" ? "ok" : "bad"}>{st}</Badge>
                      </li>
                    ))}
                  </ul>
                </>
              )}
            </Card>

            {data.info.status === "fulfilled" ? (
              <>
                <Card title="Server" icon={ServerStack01Icon}>
                  <dl>
                    <Row label="Name" value={data.info.value.name} />
                    <Row label="Version" value={data.info.value.version} mono />
                    <Row label="Mode" value={data.info.value.mode} />
                    <Row label="Uptime" value={formatUptime(data.info.value.uptime_secs)} />
                    <Row label="Started" value={<When iso={data.info.value.started_at} />} />
                    <PathRow label="Data dir" path={data.info.value.data_dir} />
                  </dl>
                </Card>
                <DatabaseCard info={data.info.value} />
                <TlsCard tls={data.info.value.tls} />
                <TracingCard />
              </>
            ) : (
              <Card title="Server" icon={ServerStack01Icon}>
                <Failed r={data.info} />
              </Card>
            )}

            <Card title="More about this server" icon={Link01Icon}>
              <ul className="divide-y divide-border/60 text-sm">
                {MORE.map((m) => (
                  <li key={m.to} className="flex items-baseline justify-between gap-3 py-2 first:pt-0 last:pb-0">
                    <span className="min-w-0">
                      <Link className="font-medium text-primary underline-offset-2 hover:underline" to={m.to}>{m.label}</Link>
                      <span className="block text-xs text-muted-foreground">{m.text}</span>
                    </span>
                    <HugeiconsIcon icon={ArrowRight01Icon} size={14} strokeWidth={1.8} aria-hidden="true" className="shrink-0 text-muted-foreground" />
                  </li>
                ))}
              </ul>
            </Card>
          </div>
        </>
      )}
    </div>
  );
}

const MORE = [
  { to: "/system/backups", label: "Backups", text: "Take, verify and prune snapshots of your data." },
  { to: "/system/topology", label: "Database topology", text: "Read replicas and shards, with their health." },
  { to: "/auth/audit", label: "Audit log", text: "Who signed in and who changed what." },
  { to: "/system/features", label: "Features and settings", text: "Switch features on or off and edit the server's settings." },
];
