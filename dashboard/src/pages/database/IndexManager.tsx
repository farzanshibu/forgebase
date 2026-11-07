import { useState } from "react";
import { analyzeIndexes, createIndex, dropIndex, indexRecommendations, indexStats, listIndexes, rebuildIndex } from "../../api";
import type { IndexDefinition, IndexStats } from "../../api";
import { useLoad } from "../../hooks";
import { Button, ConfirmButton, ErrorView, Loading, Panel, inputCls } from "../../components/ui";
import { formatBytes } from "../../format";

/** Indexes of one collection, or of every collection when `collection` is not given. */
export default function IndexManager({ collection }: { collection?: string }) {
  const { data, error, loading, reload } = useLoad((s) => listIndexes(collection, s), `indexes/${collection ?? "*"}`);
  const recs = useLoad(indexRecommendations, "recommendations");
  const [problem, setProblem] = useState<unknown>();
  const [stats, setStats] = useState<Record<string, IndexStats>>({});
  const [coll, setColl] = useState(collection ?? "");
  const [keys, setKeys] = useState("");
  const [unique, setUnique] = useState(false);
  const [indexName, setIndexName] = useState("");

  async function act(fn: () => Promise<unknown>) {
    setProblem(undefined);
    try {
      await fn();
      reload();
      recs.reload();
    } catch (e) {
      setProblem(e);
    }
  }

  const create = (def: IndexDefinition) => act(() => createIndex(def));
  const parseKeys = (s: string) =>
    s.split(",").map((k) => k.trim()).filter(Boolean).map((k) => (k.startsWith("-") ? { path: k.slice(1), desc: true } : { path: k }));

  return (
    <div className="space-y-4">
      <ErrorView error={error} />
      <ErrorView error={problem} />
      {loading && !data && !error && <Loading what="Loading indexes" />}
      {data && (
        <table className="w-full text-left text-sm">
          <thead className="text-xs uppercase tracking-wide text-zinc-500">
            <tr><th className="py-1">Name</th><th>Collection</th><th>Origin</th><th>Columns</th><th>Rows</th><th>Size</th><th /></tr>
          </thead>
          <tbody>
            {data.length === 0 && <tr><td colSpan={7} className="py-3 text-zinc-400">No indexes.</td></tr>}
            {data.map((ix) => {
              const st = stats[ix.name] ?? ix.stats;
              return (
                <tr key={ix.name} className="border-t border-zinc-800">
                  <td className="py-1 font-mono text-xs">{ix.name}{ix.unique && <span className="ml-1 text-amber-300">unique</span>}</td>
                  <td>{ix.collection ?? ix.table}</td>
                  <td>{ix.origin}</td>
                  <td className="font-mono text-xs">{ix.columns.join(", ")}</td>
                  <td>{st.rows_estimate ?? "–"}</td>
                  <td>{st.size_bytes != null ? formatBytes(st.size_bytes) : "–"}</td>
                  <td className="space-x-2 whitespace-nowrap text-right">
                    <Button onClick={() => void act(async () => { const st = await indexStats(ix.name); setStats((m) => ({ ...m, [ix.name]: st })); })}>Stats</Button>
                    <Button onClick={() => void act(() => rebuildIndex(ix.name))}>Rebuild</Button>
                    {(ix.origin === "manual" || ix.origin === "sql") && <ConfirmButton label="Drop" onConfirm={() => void act(() => dropIndex(ix.name))} />}
                  </td>
                </tr>
              );
            })}
          </tbody>
        </table>
      )}
      <Button onClick={() => void act(() => analyzeIndexes(collection))}>Analyze (refresh statistics)</Button>

      <Panel title="Create index">
        <form
          className="flex flex-wrap items-end gap-3 text-sm"
          onSubmit={(e) => {
            e.preventDefault();
            void create({ name: indexName || undefined, collection: coll, keys: parseKeys(keys), unique });
          }}
        >
          {!collection && <label className="flex flex-col gap-1">Collection<input className={inputCls} value={coll} onChange={(e) => setColl(e.target.value)} /></label>}
          <label className="flex flex-col gap-1">Keys (comma separated, prefix - for descending)<input className={`${inputCls} w-64`} value={keys} onChange={(e) => setKeys(e.target.value)} placeholder="price, -created" /></label>
          <label className="flex flex-col gap-1">Name (optional)<input className={inputCls} value={indexName} onChange={(e) => setIndexName(e.target.value)} /></label>
          <label className="flex items-center gap-2 pb-2"><input type="checkbox" checked={unique} onChange={(e) => setUnique(e.target.checked)} />Unique</label>
          <Button type="submit" tone="primary">Create index</Button>
        </form>
      </Panel>

      <Panel title="Recommendations from the slow-query log">
        <ErrorView error={recs.error} />
        {recs.data?.length === 0 && <p className="text-sm text-zinc-400">No recommendations.</p>}
        <ul className="space-y-2 text-sm">
          {recs.data?.filter((r) => !collection || r.definition.collection === collection).map((r, i) => (
            <li key={i} className="flex items-center justify-between gap-3">
              <span>
                <span className="font-mono text-xs">{r.definition.collection}({r.definition.keys.map((k) => (k.desc ? `-${k.path}` : k.path)).join(", ")})</span>
                <span className="ml-2 text-zinc-400">{r.reason} · seen {r.occurrences}×, up to {r.max_duration_ms} ms</span>
              </span>
              <Button onClick={() => void create(r.definition)}>Create</Button>
            </li>
          ))}
        </ul>
      </Panel>
    </div>
  );
}
