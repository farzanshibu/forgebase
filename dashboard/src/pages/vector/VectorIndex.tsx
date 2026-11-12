import { useState } from "react";
import { measureRecall, rebuildVectorIndex, vectorCollectionStats } from "../../api";
import type { IndexStatus, VectorCollection } from "../../api";
import { Badge, type BadgeVariant } from "panelui-web/badge";
import { SkeletonCards } from "../../components/Skeleton";
import { Button, ErrorView } from "../../components/ui";
import { Facts } from "../../components/Facts";
import { count } from "../functions/util";
import { usePolling } from "../../hooks";
import { bytes, fmt } from "./util";

export default function VectorIndex({ collection }: { collection: VectorCollection }) {
  const { data, error, refresh } = usePolling((s) => vectorCollectionStats(collection.name, s), 3000);
  const [fail, setFail] = useState<unknown>();
  const [recall, setRecall] = useState<string>();

  async function act(fn: () => Promise<unknown>) {
    setFail(undefined);
    try {
      await fn();
      refresh();
    } catch (e) {
      setFail(e);
    }
  }

  return (
    <div className="space-y-4">
      <ErrorView error={fail ?? error} />
      {!data && !error && <SkeletonCards count={2} label="Loading index status" />}
      {data && (
        <Facts
          label="Index totals"
          items={[
            { label: "Items", value: count(data.items) },
            { label: "With text", value: count(data.items_with_text) },
            { label: "Change sequence", value: String(data.change_seq), mono: true },
          ]}
        />
      )}
      {recall && <p role="status" className="rounded-lg border border-success/40 bg-success/5 px-3 py-2 text-sm text-success">{recall}</p>}
      {data?.indexes.map((ix) => (
        <IndexCard key={ix.field} ix={ix}
          onRebuild={() => void act(() => rebuildVectorIndex(collection.name, ix.field))}
          onRecall={() => void act(async () => { const r = await measureRecall(collection.name, { field: ix.field, samples: 20, k: 10 }); setRecall(`${ix.field}: recall ${fmt(r.recall)} over ${r.samples} samples (ANN ${fmt(r.ann_ms, 2)} ms, exact ${fmt(r.exact_ms, 2)} ms)`); })}
        />
      ))}
    </div>
  );
}

const STATE_TONE: Record<string, BadgeVariant> = { ready: "success", building: "info", unloaded: "secondary", failed: "destructive" };

function IndexCard({ ix, onRebuild, onRecall }: { ix: IndexStatus; onRebuild: () => void; onRecall: () => void }) {
  const pct = ix.progress && ix.progress.total > 0 ? Math.round((ix.progress.done / ix.progress.total) * 100) : null;
  return (
    <section aria-label={`Index ${ix.field}`} className="overflow-hidden rounded-xl border border-border bg-card">
      <header className="flex flex-wrap items-center justify-between gap-3 border-b border-border px-4 py-3">
        <div className="flex min-w-0 flex-wrap items-center gap-2 text-sm">
          <h2 className="font-medium">Index <span className="font-mono">{ix.field}</span></h2>
          <Badge variant={STATE_TONE[ix.state] ?? "secondary"} size="sm" dot={ix.state === "building" ? "pulse" : true}><span data-testid={`state-${ix.field}`}>{ix.state}</span></Badge>
          <span className="text-xs text-muted-foreground">serving {ix.serving}{ix.reason ? ` · ${ix.reason}` : ""}{ix.progress ? ` · ${ix.progress.done}/${ix.progress.total}` : ""}</span>
        </div>
        <div className="flex gap-2"><Button onClick={onRecall}>Measure recall</Button><Button tone="primary" onClick={onRebuild} disabled={ix.state === "building"}>Rebuild {ix.field}</Button></div>
      </header>
      {pct !== null && (
        <div role="progressbar" aria-label={`Building ${ix.field}`} aria-valuemin={0} aria-valuemax={100} aria-valuenow={pct} className="h-1 w-full bg-muted">
          <div className="h-full bg-primary transition-all" style={{ width: `${pct}%` }} />
        </div>
      )}
      <dl className="grid grid-cols-2 gap-x-6 gap-y-3 px-4 py-3 text-sm md:grid-cols-4">
        <Stat k="Indexed / canonical" v={`${ix.indexed_vectors} / ${ix.canonical_vectors}`} />
        <Stat k="Memory" v={bytes(ix.memory_bytes)} />
        <Stat k="Engine" v={`${ix.engine} ${ix.engine_version}`} />
        <Stat k="Quantization" v={ix.quantization} />
        <Stat k="Connectivity" v={String(ix.connectivity)} />
        <Stat k="Expansion search" v={String(ix.expansion_search)} />
        <Stat k="Searches" v={`${ix.searches} (${ix.ann_searches} ANN, ${ix.exact_searches} exact)`} />
        <Stat k="Mean search" v={`${fmt(ix.mean_search_ms, 2)} ms`} />
        {ix.last_rebuild && <Stat k="Last rebuild" v={`${ix.last_rebuild.reason}, ${ix.last_rebuild.duration_ms} ms`} />}
        {ix.last_recall && <Stat k="Last recall" v={fmt(ix.last_recall.recall)} />}
      </dl>
    </section>
  );
}

const Stat = ({ k, v }: { k: string; v: string }) => (
  <div className="min-w-0"><dt className="text-xs text-muted-foreground">{k}</dt><dd className="mt-0.5 truncate font-medium tabular-nums" title={v}>{v}</dd></div>
);
