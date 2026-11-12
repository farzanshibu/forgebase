import { kvInfo } from "../../api";
import type { KvNamespace, KvStats } from "../../api";
import { EmptyState } from "../../components/EmptyState";
import { SkeletonLines } from "../../components/Skeleton";
import { Button, ErrorView } from "../../components/ui";
import { formatBytes } from "../../format";
import { usePolling } from "../../hooks";
import { Table } from "panelui-web/table";

/** `_info` also reports whose figures these are (FB-138): the whole `engine`, or one `tenant`. */
type Scoped = KvStats & { scope?: "engine" | "tenant" | string; tenant?: string | null };

export default function StatsPanel({ namespaces }: { ns: string; namespaces: KvNamespace[] }) {
  const s = usePolling<Scoped>(kvInfo, 5000);
  const d = s.data;
  const reads = d ? d.hits + d.misses : 0;
  const rate = d && reads > 0 ? Math.round((d.hits / reads) * 1000) / 10 : null;
  const pct = d && d.max_memory_bytes > 0 ? Math.min(100, Math.round((d.used_memory_bytes / d.max_memory_bytes) * 100)) : null;
  const card = "overflow-hidden rounded-xl border border-border bg-card";
  const head = "flex items-center justify-between gap-2 border-b border-border px-4 py-2.5";
  return (
    <div className="space-y-4">
      <section aria-labelledby="kv-stats-memory" className={card}>
        <header className={head}>
          <h2 id="kv-stats-memory" className="text-sm font-medium">Memory and cache</h2>
          <span className="flex items-center gap-3">
            {s.updatedAt && <span className="text-xs text-muted-foreground">Updated {s.updatedAt.toLocaleTimeString()} · every 5 s</span>}
            <Button onClick={s.refresh}>Refresh</Button>
          </span>
        </header>
        <div className="space-y-4 p-4">
          <ErrorView error={s.error} />
          {s.loading && !s.error && <SkeletonLines lines={4} label="Loading statistics" />}
          {d && (
            <div className="space-y-4 text-sm">
              {d.scope === "tenant" && <p role="status" className="rounded-lg bg-muted px-3 py-2 text-xs">Figures for tenant {d.tenant ?? ""} only. The per-tenant memory budget (<code className="font-mono">kv.tenant_max_bytes</code>) evicts only that tenant&apos;s keys.</p>}
              <div className="space-y-2">
                <p aria-label="Memory" className="flex flex-wrap items-baseline gap-x-2">
                  <span className="text-2xl font-semibold tabular-nums tracking-tight">{formatBytes(d.used_memory_bytes)}</span>{" "}
                  <span className="text-muted-foreground">used{d.max_memory_bytes > 0 ? ` of ${formatBytes(d.max_memory_bytes)} (${pct}%)` : " (no memory limit)"} · eviction policy <span className="font-mono">{d.eviction_policy}</span></span>
                </p>
                {pct !== null && (
                  <div role="progressbar" aria-label="Memory budget" aria-valuenow={pct} aria-valuemin={0} aria-valuemax={100} className="h-2 w-full overflow-hidden rounded-full bg-muted">
                    <div className={`h-full rounded-full ${pct >= 90 ? "bg-destructive" : pct >= 70 ? "bg-warning" : "bg-success"}`} style={{ width: `${pct}%` }} />
                  </div>
                )}
              </div>
              <dl aria-label="Cache statistics" className="grid grid-cols-2 gap-3 sm:grid-cols-4">
                {([["Keys", d.keys], ["Hit rate", rate === null ? "n/a" : `${rate}%`], ["Hits", d.hits], ["Misses", d.misses], ["Expired keys", d.expired_keys], ["Evicted keys", d.evicted_keys], ["Pub/Sub channels", d.channels], ["Unsaved keys", d.unsaved_keys]] as [string, string | number][]).map(([k, v]) => (
                  <div key={k} className="rounded-lg border border-border px-3 py-2.5"><dt className="text-xs text-muted-foreground">{k}</dt><dd className="mt-0.5 text-lg font-semibold tabular-nums">{typeof v === "number" ? v.toLocaleString("en-US") : v}</dd></div>
                ))}
              </dl>
              <p className="text-xs text-muted-foreground">Persistence: {d.persistent ? "changes are written to SQLite" : "memory only"}.</p>
            </div>
          )}
        </div>
      </section>
      <section aria-labelledby="kv-stats-ns" className={card}>
        <header className={head}>
          <h2 id="kv-stats-ns" className="text-sm font-medium">Namespaces</h2>
          {namespaces.length > 0 && <span className="text-xs tabular-nums text-muted-foreground">{namespaces.length}</span>}
        </header>
        {namespaces.length === 0 ? <div className="p-4"><EmptyState compact title="No namespace holds keys" why="A namespace appears here after its first key is written. Names starting with shared. are shared by every tenant of a realm." /></div> : (
          <Table aria-label="Namespaces" framed density="compact" className="w-full text-left text-sm">
            <Table.Header className="text-xs text-muted-foreground"><Table.Row><Table.Head>Namespace</Table.Head><Table.Head align="end">Keys</Table.Head></Table.Row></Table.Header>
            <Table.Body>{namespaces.map((n) => <Table.Row key={n.name} className="border-t border-border hover:bg-muted/40"><Table.Cell className="font-mono">{n.name}</Table.Cell><Table.Cell align="end">{n.keys.toLocaleString("en-US")}</Table.Cell></Table.Row>)}</Table.Body>
          </Table>
        )}
      </section>
    </div>
  );
}
