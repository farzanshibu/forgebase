import { useState } from "react";
import { vectorHybrid, vectorSearch } from "../../api";
import type { FilterMode, HybridResponse, SearchResponse, VectorCollection } from "../../api";
import { Tabs } from "panelui-web/tabs";
import { EmptyState } from "../../components/EmptyState";
import { FormField } from "../../components/FormField";
import { Button, ErrorView } from "../../components/ui";
import { fmt, parseVector } from "./util";
import { Table } from "panelui-web/table";
import { Input } from "panelui-web/input";
import { Check, Sel } from "../../components/controls";
import { HelpTip } from "../../components/HelpTip";

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

  const dim = collection.fields.find((f) => f.name === field)?.dimension ?? 3;
  const placeholder = Array.from({ length: Math.min(dim, 4) }, (_, i) => (0.1 * (i + 1)).toFixed(1)).join(", ") + (dim > 4 ? ", …" : "");
  const card = "overflow-hidden rounded-xl border border-border bg-card";
  const head = "flex flex-wrap items-center justify-between gap-2 border-b border-border px-4 py-2.5";
  return (
    <div className="space-y-4">
      <section aria-labelledby="vs-query" className={card}>
        <header className={head}>
          <h2 id="vs-query" className="text-sm font-medium">Search playground</h2>
          <Tabs variant="segmented" value={mode} onValueChange={(v) => setMode(v as Mode)}>
            <Tabs.List aria-label="Search mode">
              <Tabs.Trigger value="similarity">Similarity</Tabs.Trigger>
              <Tabs.Trigger value="hybrid">Hybrid</Tabs.Trigger>
            </Tabs.List>
          </Tabs>
        </header>
        <form aria-label="Search" onSubmit={(e) => void run(e)} className="grid gap-3 p-4 md:grid-cols-4">
          <FormField label="Field">
            {(p) => <Sel {...p} aria-label="Field" className="w-full" value={field} onChange={setField} options={collection.fields.map((f) => ({ value: f.name, label: `${f.name} (${f.dimension})` }))} />}
          </FormField>
          <FormField label="Query vector" className="md:col-span-3" help={mode === "hybrid" ? `Optional in hybrid search. ${dim} numbers.` : `${dim} numbers, separated by commas or as a JSON array.`}>
            {(p) => <Input {...p} size="sm" className="w-full font-mono" value={vector} onChange={(e) => setVector(e.target.value)} placeholder={placeholder} />}
          </FormField>
          {mode === "hybrid" && (
            <>
              <FormField label="Query text" className="md:col-span-2" help="Keywords matched against each record's text.">
                {(p) => <Input {...p} size="sm" className="w-full" value={text} onChange={(e) => setText(e.target.value)} />}
              </FormField>
              <FormField label="Text operator">
                {(p) => <Sel {...p} aria-label="Text operator" className="w-full" value={operator} onChange={(v) => setOperator(v as "or" | "and")} options={[{ value: "or", label: "or (any word)" }, { value: "and", label: "and (every word)" }]} />}
              </FormField>
              <div className="grid grid-cols-2 gap-3">
                <FormField label="Vector weight">{(p) => <Input {...p} size="sm" type="number" step="any" className="w-full tabular-nums" value={wVec} onChange={(e) => setWVec(e.target.value)} />}</FormField>
                <FormField label="Text weight">{(p) => <Input {...p} size="sm" type="number" step="any" className="w-full tabular-nums" value={wText} onChange={(e) => setWText(e.target.value)} />}</FormField>
              </div>
            </>
          )}
          <FormField label="Metadata filter" className="md:col-span-2" help="Optional, such as tag = &quot;a&quot; &amp;&amp; year > 2020.">
            {(p) => <Input {...p} size="sm" className="w-full font-mono" value={filter} onChange={(e) => setFilter(e.target.value)} placeholder='tag = "a" && year > 2020' />}
          </FormField>
          <FormField label="Filter mode" help="Auto picks pre or post filtering.">
            {(p) => <Sel {...p} aria-label="Filter mode" className="w-full" value={filterMode} onChange={(v) => setFilterMode(v as FilterMode)} options={[{ value: "auto", label: "auto" }, { value: "pre", label: "pre" }, { value: "post", label: "post" }]} />}
          </FormField>
          <FormField label="k" help="How many results, 1 to 1000.">
            {(p) => <Input {...p} size="sm" type="number" min={1} max={1000} className="w-full tabular-nums" value={k} onChange={(e) => setK(e.target.value)} />}
          </FormField>
          <div className="flex flex-wrap items-center gap-4 md:col-span-4">
            <Button tone="primary" type="submit">Run search</Button>
            {mode === "similarity" && <span className="flex items-center gap-1 text-sm"><Check checked={exact} onChange={setExact} label="Exact (brute force)" /><HelpTip term="exact_search" /></span>}
          </div>
        </form>
      </section>
      <ErrorView error={error} />
      {sim && (
        <section aria-labelledby="vs-hits" className={card}>
          <header className={head}>
            <h2 id="vs-hits" className="text-sm font-medium">{`${sim.hits.length} hits`}</h2>
            <p aria-label="Search summary" className="text-xs text-muted-foreground">strategy <span className="font-mono">{sim.strategy}</span> · {sim.candidates ?? "n/a"} candidates · {fmt(sim.took_ms, 2)} ms</p>
          </header>
          {sim.hits.length === 0 ? <div className="p-4"><EmptyState compact title="No hits" why="Nothing matched. Loosen the metadata filter or raise k." /></div> : (
            <Table framed density="compact" className="w-full text-left text-sm">
              <Table.Header className="text-xs text-muted-foreground"><Table.Row><Table.Head align="end" className="w-12">#</Table.Head><Table.Head>Id</Table.Head><Table.Head align="end">Distance</Table.Head><Table.Head align="end">Score</Table.Head><Table.Head>Metadata</Table.Head></Table.Row></Table.Header>
              <Table.Body>
                {sim.hits.map((h, i) => (
                  <Table.Row key={h.id} className="border-t border-border hover:bg-muted/40">
                    <Table.Cell align="end" className="text-muted-foreground">{i + 1}</Table.Cell>
                    <Table.Cell className="max-w-56 truncate font-mono" title={h.id}>{h.id}</Table.Cell>
                    <Table.Cell align="end" className="font-mono">{fmt(h.distance)}</Table.Cell>
                    <Table.Cell align="end" className="font-mono">{fmt(h.score)}</Table.Cell>
                    <Table.Cell className="max-w-sm truncate font-mono text-xs text-muted-foreground" title={JSON.stringify(h.metadata ?? {})}>{JSON.stringify(h.metadata ?? {})}</Table.Cell>
                  </Table.Row>
                ))}
              </Table.Body>
            </Table>
          )}
        </section>
      )}
      {hyb && (
        <section aria-labelledby="vs-hyb" className={card}>
          <header className={head}>
            <h2 id="vs-hyb" className="text-sm font-medium">{`${hyb.hits.length} hybrid hits`}</h2>
            <p aria-label="Search summary" className="text-xs text-muted-foreground">
              vector strategy <span className="font-mono">{hyb.vector_strategy ?? "none"}</span> · {hyb.vector_candidates} vector candidates · {hyb.text_candidates} text candidates · {fmt(hyb.took_ms, 2)} ms
            </p>
          </header>
          {hyb.hits.length === 0 ? <div className="p-4"><EmptyState compact title="No hits" why="Nothing matched the vector or the text. Try other words or a looser filter." /></div> : (
            <Table framed density="compact" className="w-full text-left text-sm">
              <Table.Header className="text-xs text-muted-foreground">
                <Table.Row><Table.Head>Id</Table.Head><Table.Head align="end">Fused score</Table.Head><Table.Head align="end">Vector rank</Table.Head><Table.Head align="end">Vector distance</Table.Head><Table.Head align="end">Text rank</Table.Head><Table.Head align="end">Text score</Table.Head></Table.Row>
              </Table.Header>
              <Table.Body>
                {hyb.hits.map((h) => (
                  <Table.Row key={h.id} className="border-t border-border hover:bg-muted/40">
                    <Table.Cell className="max-w-56 truncate font-mono" title={h.id}>{h.id}</Table.Cell><Table.Cell align="end" className="font-mono">{fmt(h.score, 6)}</Table.Cell>
                    <Table.Cell align="end">{h.vector_rank ?? "–"}</Table.Cell><Table.Cell align="end" className="font-mono">{h.vector_distance === undefined ? "–" : fmt(h.vector_distance)}</Table.Cell>
                    <Table.Cell align="end">{h.text_rank ?? "–"}</Table.Cell><Table.Cell align="end" className="font-mono">{h.text_score === undefined ? "–" : fmt(h.text_score)}</Table.Cell>
                  </Table.Row>
                ))}
              </Table.Body>
            </Table>
          )}
        </section>
      )}
    </div>
  );
}
