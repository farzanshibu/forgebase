import { useState } from "react";
import { vectorHybrid, vectorSearch } from "../../api";
import type { FilterMode, HybridResponse, SearchResponse, VectorCollection } from "../../api";
import { Button, ErrorView, Panel, inputCls } from "../../components/ui";
import { fmt, parseVector } from "./util";

type Mode = "similarity" | "hybrid";

export default function VectorSearch({ collection }: { collection: VectorCollection }) {
  const [mode, setMode] = useState<Mode>("similarity");
  const [field, setField] = useState(collection.fields[0]?.name ?? "");
  const [vector, setVector] = useState("");
  const [text, setText] = useState("");
  const [filter, setFilter] = useState("");
  const [filterMode, setFilterMode] = useState<FilterMode>("auto");
  const [k, setK] = useState("10");
  const [exact, setExact] = useState(false);
  const [operator, setOperator] = useState<"or" | "and">("or");
  const [wVec, setWVec] = useState("1");
  const [wText, setWText] = useState("1");
  const [error, setError] = useState<unknown>();
  const [sim, setSim] = useState<SearchResponse>();
  const [hyb, setHyb] = useState<HybridResponse>();

  async function run(e: React.FormEvent) {
    e.preventDefault();
    setError(undefined);
    setSim(undefined);
    setHyb(undefined);
    try {
      const common = { field, k: Number(k), ...(filter.trim() ? { filter, filter_mode: filterMode } : {}), include_text: true };
      if (mode === "similarity") setSim(await vectorSearch(collection.name, { ...common, vector: parseVector(vector), exact }));
      else {
        if (!vector.trim() && !text.trim()) throw new Error("Hybrid search needs a vector, text, or both");
        setHyb(await vectorHybrid(collection.name, {
          ...common, ...(vector.trim() ? { vector: parseVector(vector) } : {}), ...(text.trim() ? { text, text_operator: operator } : {}),
          weights: { vector: Number(wVec), text: Number(wText) },
        }));
      }
    } catch (err) {
      setError(err);
    }
  }

  return (
    <div className="space-y-4">
      <Panel title="Search playground">
        <div role="tablist" aria-label="Search mode" className="mb-3 flex gap-2">
          {(["similarity", "hybrid"] as const).map((m) => (
            <button key={m} type="button" role="tab" aria-selected={mode === m} onClick={() => setMode(m)} className={`rounded-md border px-3 py-1 text-sm ${mode === m ? "border-amber-500 text-amber-300" : "border-zinc-700 text-zinc-400"}`}>
              {m === "similarity" ? "Similarity" : "Hybrid"}
            </button>
          ))}
        </div>
        <form aria-label="Search" onSubmit={(e) => void run(e)} className="grid gap-3 md:grid-cols-3">
          <label className="text-sm">Field
            <select className={`${inputCls} mt-1 w-full`} value={field} onChange={(e) => setField(e.target.value)}>
              {collection.fields.map((f) => <option key={f.name} value={f.name}>{f.name}</option>)}
            </select>
          </label>
          <label className="text-sm md:col-span-2">Query vector<input className={`${inputCls} mt-1 w-full font-mono`} value={vector} onChange={(e) => setVector(e.target.value)} placeholder="0.1, 0.2, 0.3" /></label>
          {mode === "hybrid" && (
            <>
              <label className="text-sm md:col-span-2">Query text<input className={`${inputCls} mt-1 w-full`} value={text} onChange={(e) => setText(e.target.value)} /></label>
              <label className="text-sm">Text operator
                <select className={`${inputCls} mt-1 w-full`} value={operator} onChange={(e) => setOperator(e.target.value as "or" | "and")}>
                  <option value="or">or</option><option value="and">and</option>
                </select>
              </label>
              <label className="text-sm">Vector weight<input type="number" step="any" className={`${inputCls} mt-1 w-full`} value={wVec} onChange={(e) => setWVec(e.target.value)} /></label>
              <label className="text-sm">Text weight<input type="number" step="any" className={`${inputCls} mt-1 w-full`} value={wText} onChange={(e) => setWText(e.target.value)} /></label>
            </>
          )}
          <label className="text-sm md:col-span-2">Metadata filter<input className={`${inputCls} mt-1 w-full font-mono`} value={filter} onChange={(e) => setFilter(e.target.value)} placeholder='tag = "a" && year > 2020' /></label>
          <label className="text-sm">Filter mode
            <select className={`${inputCls} mt-1 w-full`} value={filterMode} onChange={(e) => setFilterMode(e.target.value as FilterMode)}>
              <option value="auto">auto</option><option value="pre">pre</option><option value="post">post</option>
            </select>
          </label>
          <label className="text-sm">k<input type="number" min={1} max={1000} className={`${inputCls} mt-1 w-full`} value={k} onChange={(e) => setK(e.target.value)} /></label>
          {mode === "similarity" && (
            <label className="flex items-end gap-2 text-sm"><input type="checkbox" checked={exact} onChange={(e) => setExact(e.target.checked)} />Exact (brute force)</label>
          )}
          <div className="flex items-end"><Button tone="primary" type="submit">Run search</Button></div>
        </form>
      </Panel>
      <ErrorView error={error} />
      {sim && (
        <Panel title={`${sim.hits.length} hits`}>
          <p aria-label="Search summary" className="mb-2 text-xs text-zinc-400">strategy {sim.strategy} · {sim.candidates ?? "n/a"} candidates · {fmt(sim.took_ms, 2)} ms</p>
          <table className="w-full text-left text-sm">
            <thead className="text-xs uppercase text-zinc-500"><tr><th className="py-1">Id</th><th>Distance</th><th>Score</th><th>Metadata</th></tr></thead>
            <tbody>
              {sim.hits.map((h) => (
                <tr key={h.id} className="border-t border-zinc-800">
                  <td className="py-1 font-mono">{h.id}</td><td>{fmt(h.distance)}</td><td>{fmt(h.score)}</td>
                  <td className="font-mono text-xs text-zinc-400">{JSON.stringify(h.metadata ?? {})}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </Panel>
      )}
      {hyb && (
        <Panel title={`${hyb.hits.length} hybrid hits`}>
          <p aria-label="Search summary" className="mb-2 text-xs text-zinc-400">
            vector strategy {hyb.vector_strategy ?? "none"} · {hyb.vector_candidates} vector candidates · {hyb.text_candidates} text candidates · {fmt(hyb.took_ms, 2)} ms
          </p>
          <table className="w-full text-left text-sm">
            <thead className="text-xs uppercase text-zinc-500">
              <tr><th className="py-1">Id</th><th>Fused score</th><th>Vector rank</th><th>Vector distance</th><th>Text rank</th><th>Text score</th></tr>
            </thead>
            <tbody>
              {hyb.hits.map((h) => (
                <tr key={h.id} className="border-t border-zinc-800">
                  <td className="py-1 font-mono">{h.id}</td><td>{fmt(h.score, 6)}</td>
                  <td>{h.vector_rank ?? "–"}</td><td>{h.vector_distance === undefined ? "–" : fmt(h.vector_distance)}</td>
                  <td>{h.text_rank ?? "–"}</td><td>{h.text_score === undefined ? "–" : fmt(h.text_score)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </Panel>
      )}
    </div>
  );
}
