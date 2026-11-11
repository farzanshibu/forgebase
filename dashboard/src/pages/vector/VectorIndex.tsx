import { useState } from "react";
import { measureRecall, rebuildVectorIndex, vectorCollectionStats } from "../../api";
import type { IndexStatus, VectorCollection } from "../../api";
import { SkeletonLines } from "../../components/Skeleton";
import { Button, ErrorView, Panel } from "../../components/ui";
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
      {!data && !error && <SkeletonLines lines={3} label="Loading index status" />}
      {data && (
        <p className="text-sm text-muted-foreground">
          {data.items} items ({data.items_with_text} with text) · change seq {data.change_seq}
        </p>
      )}
      {data?.indexes.map((ix) => (
        <IndexCard key={ix.field} ix={ix}
          onRebuild={() => void act(() => rebuildVectorIndex(collection.name, ix.field))}
          onRecall={() => void act(async () => { const r = await measureRecall(collection.name, { field: ix.field, samples: 20, k: 10 }); setRecall(`${ix.field}: recall ${fmt(r.recall)} over ${r.samples} samples (ANN ${fmt(r.ann_ms, 2)} ms, exact ${fmt(r.exact_ms, 2)} ms)`); })}
        />
      ))}
      {recall && <p role="status" className="text-sm text-success">{recall}</p>}
    </div>
  );
}

function IndexCard({ ix, onRebuild, onRecall }: { ix: IndexStatus; onRebuild: () => void; onRecall: () => void }) {
  return (
    <Panel
      title={`Index ${ix.field}`}
      actions={<div className="flex gap-2"><Button onClick={onRecall}>Measure recall</Button><Button tone="primary" onClick={onRebuild} disabled={ix.state === "building"}>Rebuild {ix.field}</Button></div>}
    >
      <p className="text-sm">
        State <span data-testid={`state-${ix.field}`} className="font-medium text-warning">{ix.state}</span> · serving {ix.serving}
        {ix.reason ? ` · ${ix.reason}` : ""}
        {ix.progress ? ` · ${ix.progress.done}/${ix.progress.total}` : ""}
      </p>
      <dl className="mt-2 grid grid-cols-2 gap-x-6 gap-y-1 text-sm md:grid-cols-4">
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
    </Panel>
  );
}

const Stat = ({ k, v }: { k: string; v: string }) => (
  <div><dt className="text-xs text-muted-foreground">{k}</dt><dd>{v}</dd></div>
);
