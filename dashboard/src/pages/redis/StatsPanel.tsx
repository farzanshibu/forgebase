import { kvInfo } from "../../api";
import type { KvNamespace } from "../../api";
import { Button, ErrorView, Loading, Panel } from "../../components/ui";
import { formatBytes } from "../../format";
import { usePolling } from "../../hooks";

export default function StatsPanel({ namespaces }: { ns: string; namespaces: KvNamespace[] }) {
  const s = usePolling(kvInfo, 5000);
  const d = s.data;
  const reads = d ? d.hits + d.misses : 0;
  const rate = d && reads > 0 ? Math.round((d.hits / reads) * 1000) / 10 : null;
  const pct = d && d.max_memory_bytes > 0 ? Math.min(100, Math.round((d.used_memory_bytes / d.max_memory_bytes) * 100)) : null;
  return (
    <div className="space-y-4">
      <Panel title="Memory and cache" actions={<Button onClick={s.refresh}>Refresh</Button>}>
        <ErrorView error={s.error} />
        {s.loading && !s.error && <Loading what="Loading statistics" />}
        {d && (
          <div className="space-y-3 text-sm">
            <p aria-label="Memory">{formatBytes(d.used_memory_bytes)} used{d.max_memory_bytes > 0 ? ` of ${formatBytes(d.max_memory_bytes)} (${pct}%)` : " (no memory limit)"} · eviction policy {d.eviction_policy}</p>
            {pct !== null && (
              <div role="progressbar" aria-label="Memory budget" aria-valuenow={pct} aria-valuemin={0} aria-valuemax={100} className="h-2 w-full overflow-hidden rounded bg-zinc-800">
                <div className={`h-full ${pct >= 90 ? "bg-red-500" : pct >= 70 ? "bg-amber-400" : "bg-emerald-500"}`} style={{ width: `${pct}%` }} />
              </div>
            )}
            <dl aria-label="Cache statistics" className="grid grid-cols-2 gap-x-6 gap-y-1 sm:grid-cols-4">
              {([["Keys", d.keys], ["Hit rate", rate === null ? "n/a" : `${rate}%`], ["Hits", d.hits], ["Misses", d.misses], ["Expired keys", d.expired_keys], ["Evicted keys", d.evicted_keys], ["Pub/Sub channels", d.channels], ["Unsaved keys", d.unsaved_keys]] as [string, string | number][]).map(([k, v]) => (
                <div key={k}><dt className="text-xs uppercase tracking-wide text-zinc-500">{k}</dt><dd>{v}</dd></div>
              ))}
            </dl>
            <p className="text-zinc-400">Persistence: {d.persistent ? "changes are written to SQLite" : "memory only"}.</p>
          </div>
        )}
      </Panel>
      <Panel title="Namespaces">
        {namespaces.length === 0 ? <p className="text-sm text-zinc-400">No namespace holds keys.</p> : (
          <table aria-label="Namespaces" className="w-full text-left text-sm">
            <thead className="text-xs uppercase tracking-wide text-zinc-500"><tr><th className="py-1">Namespace</th><th>Keys</th></tr></thead>
            <tbody>{namespaces.map((n) => <tr key={n.name} className="border-t border-zinc-800"><td className="py-1.5">{n.name}</td><td>{n.keys}</td></tr>)}</tbody>
          </table>
        )}
      </Panel>
    </div>
  );
}
