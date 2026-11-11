import { kvInfo } from "../../api";
import type { KvNamespace, KvStats } from "../../api";
import { EmptyState } from "../../components/EmptyState";
import { SkeletonLines } from "../../components/Skeleton";
import { Button, ErrorView, Panel } from "../../components/ui";
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
  return (
    <div className="space-y-4">
      <Panel title="Memory and cache" actions={<Button onClick={s.refresh}>Refresh</Button>}>
        <ErrorView error={s.error} />
        {s.loading && !s.error && <SkeletonLines lines={4} label="Loading statistics" />}
        {d && (
          <div className="space-y-3 text-sm">
            {d.scope === "tenant" && <p role="status" className="rounded bg-muted px-2 py-1 text-xs">Figures for tenant {d.tenant ?? ""} only. The per-tenant memory budget (<code className="font-mono">kv.tenant_max_bytes</code>) evicts only that tenant&apos;s keys.</p>}
            <p aria-label="Memory">{formatBytes(d.used_memory_bytes)} used{d.max_memory_bytes > 0 ? ` of ${formatBytes(d.max_memory_bytes)} (${pct}%)` : " (no memory limit)"} · eviction policy {d.eviction_policy}</p>
            {pct !== null && (
              <div role="progressbar" aria-label="Memory budget" aria-valuenow={pct} aria-valuemin={0} aria-valuemax={100} className="h-2 w-full overflow-hidden rounded bg-muted">
                <div className={`h-full ${pct >= 90 ? "bg-destructive" : pct >= 70 ? "bg-primary" : "bg-success"}`} style={{ width: `${pct}%` }} />
              </div>
            )}
            <dl aria-label="Cache statistics" className="grid grid-cols-2 gap-x-6 gap-y-1 sm:grid-cols-4">
              {([["Keys", d.keys], ["Hit rate", rate === null ? "n/a" : `${rate}%`], ["Hits", d.hits], ["Misses", d.misses], ["Expired keys", d.expired_keys], ["Evicted keys", d.evicted_keys], ["Pub/Sub channels", d.channels], ["Unsaved keys", d.unsaved_keys]] as [string, string | number][]).map(([k, v]) => (
                <div key={k}><dt className="text-xs uppercase tracking-wide text-muted-foreground">{k}</dt><dd>{v}</dd></div>
              ))}
            </dl>
            <p className="text-muted-foreground">Persistence: {d.persistent ? "changes are written to SQLite" : "memory only"}.</p>
          </div>
        )}
      </Panel>
      <Panel title="Namespaces">
        {namespaces.length === 0 ? <EmptyState compact title="No namespace holds keys" why="A namespace appears here after its first key is written. Names starting with shared. are shared by every tenant of a realm." /> : (
          <Table aria-label="Namespaces" className="w-full text-left text-sm">
            <Table.Header className="text-xs uppercase tracking-wide text-muted-foreground"><Table.Row><Table.Head className="py-1">Namespace</Table.Head><Table.Head>Keys</Table.Head></Table.Row></Table.Header>
            <Table.Body>{namespaces.map((n) => <Table.Row key={n.name} className="border-t border-border"><Table.Cell className="py-1.5">{n.name}</Table.Cell><Table.Cell>{n.keys}</Table.Cell></Table.Row>)}</Table.Body>
          </Table>
        )}
      </Panel>
    </div>
  );
}
