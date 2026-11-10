import { useState } from "react";
import { vectorHybrid, vectorSearch } from "../../api";
import type { FilterMode, HybridResponse, SearchResponse, VectorCollection } from "../../api";
import { Button, ErrorView, Panel, inputCls } from "../../components/ui";
import { fmt, parseVector } from "./util";
import { Table } from "panelui-web/table";
import { Input } from "panelui-web/input";

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
            <button key={m} type="button" role="tab" aria-selected={mode === m} onClick={() => setMode(m)} className={`rounded-md border px-3 py-1 text-sm ${mode === m ? "border-primary text-warning" : "border-border text-muted-foreground"}`}>
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
          <label className="text-sm md:col-span-2">Query vector<Input size="sm" className="mt-1 w-full font-mono" value={vector} onChange={(e) => setVector(e.target.value)} placeholder="0.1, 0.2, 0.3" /></label>
          {mode === "hybrid" && (
            <>
              <label className="text-sm md:col-span-2">Query text<Input size="sm" className="mt-1 w-full" value={text} onChange={(e) => setText(e.target.value)} /></label>
              <label className="text-sm">Text operator
                <select className={`${inputCls} mt-1 w-full`} value={operator} onChange={(e) => setOperator(e.target.value as "or" | "and")}>
                  <option value="or">or</option><option value="and">and</option>
                </select>
              </label>
              <label className="text-sm">Vector weight<Input size="sm" type="number" step="any" className="mt-1 w-full" value={wVec} onChange={(e) => setWVec(e.target.value)} /></label>
              <label className="text-sm">Text weight<Input size="sm" type="number" step="any" className="mt-1 w-full" value={wText} onChange={(e) => setWText(e.target.value)} /></label>
            </>
          )}
          <label className="text-sm md:col-span-2">Metadata filter<Input size="sm" className="mt-1 w-full font-mono" value={filter} onChange={(e) => setFilter(e.target.value)} placeholder='tag = "a" && year > 2020' /></label>
          <label className="text-sm">Filter mode
            <select className={`${inputCls} mt-1 w-full`} value={filterMode} onChange={(e) => setFilterMode(e.target.value as FilterMode)}>
              <option value="auto">auto</option><option value="pre">pre</option><option value="post">post</option>
            </select>
          </label>
          <label className="text-sm">k<Input size="sm" type="number" min={1} max={1000} className="mt-1 w-full" value={k} onChange={(e) => setK(e.target.value)} /></label>
          {mode === "similarity" && (
            <label className="flex items-end gap-2 text-sm"><input type="checkbox" checked={exact} onChange={(e) => setExact(e.target.checked)} />Exact (brute force)</label>
          )}
          <div className="flex items-end"><Button tone="primary" type="submit">Run search</Button></div>
        </form>
      </Panel>
      <ErrorView error={error} />
      {sim && (
        <Panel title={`${sim.hits.length} hits`}>
          <p aria-label="Search summary" className="mb-2 text-xs text-muted-foreground">strategy {sim.strategy} · {sim.candidates ?? "n/a"} candidates · {fmt(sim.took_ms, 2)} ms</p>
          <Table className="w-full text-left text-sm">
            <Table.Header className="text-xs uppercase text-muted-foreground"><Table.Row><Table.Head className="py-1">Id</Table.Head><Table.Head>Distance</Table.Head><Table.Head>Score</Table.Head><Table.Head>Metadata</Table.Head></Table.Row></Table.Header>
            <Table.Body>
              {sim.hits.map((h) => (
                <Table.Row key={h.id} className="border-t border-border">
                  <Table.Cell className="py-1 font-mono">{h.id}</Table.Cell><Table.Cell>{fmt(h.distance)}</Table.Cell><Table.Cell>{fmt(h.score)}</Table.Cell>
                  <Table.Cell className="font-mono text-xs text-muted-foreground">{JSON.stringify(h.metadata ?? {})}</Table.Cell>
                </Table.Row>
              ))}
            </Table.Body>
          </Table>
        </Panel>
      )}
      {hyb && (
        <Panel title={`${hyb.hits.length} hybrid hits`}>
          <p aria-label="Search summary" className="mb-2 text-xs text-muted-foreground">
            vector strategy {hyb.vector_strategy ?? "none"} · {hyb.vector_candidates} vector candidates · {hyb.text_candidates} text candidates · {fmt(hyb.took_ms, 2)} ms
          </p>
          <Table className="w-full text-left text-sm">
            <Table.Header className="text-xs uppercase text-muted-foreground">
              <Table.Row><Table.Head className="py-1">Id</Table.Head><Table.Head>Fused score</Table.Head><Table.Head>Vector rank</Table.Head><Table.Head>Vector distance</Table.Head><Table.Head>Text rank</Table.Head><Table.Head>Text score</Table.Head></Table.Row>
            </Table.Header>
            <Table.Body>
              {hyb.hits.map((h) => (
                <Table.Row key={h.id} className="border-t border-border">
                  <Table.Cell className="py-1 font-mono">{h.id}</Table.Cell><Table.Cell>{fmt(h.score, 6)}</Table.Cell>
                  <Table.Cell>{h.vector_rank ?? "–"}</Table.Cell><Table.Cell>{h.vector_distance === undefined ? "–" : fmt(h.vector_distance)}</Table.Cell>
                  <Table.Cell>{h.text_rank ?? "–"}</Table.Cell><Table.Cell>{h.text_score === undefined ? "–" : fmt(h.text_score)}</Table.Cell>
                </Table.Row>
              ))}
            </Table.Body>
          </Table>
        </Panel>
      )}
    </div>
  );
}
