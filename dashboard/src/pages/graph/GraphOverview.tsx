import { useState } from "react";
import { Link, useNavigate } from "react-router-dom";
import { Input } from "panelui-web/input";
import { Table } from "panelui-web/table";
import { graphStats } from "../../api";
import { Sel } from "../../components/controls";
import { EmptyState } from "../../components/EmptyState";
import { FormField } from "../../components/FormField";
import { SkeletonLines } from "../../components/Skeleton";
import { Button, ErrorView } from "../../components/ui";
import { useLoad } from "../../hooks";
import { notify } from "../../toaster";
import ResourceRules from "../../components/ResourceRules";
import { loadGraphSample } from "./sample";

/** The API serves one graph per tenant (ADR-0006), so the graph list is that graph with its statistics. */
export default function GraphOverview() {
  const nav = useNavigate();
  const { data, error, loading, reload } = useLoad(graphStats, "graph-stats");
  const [busy, setBusy] = useState(false);
  const [sampleError, setSampleError] = useState<unknown>();
  async function sample() {
    setBusy(true);
    setSampleError(undefined);
    try {
      await loadGraphSample();
      notify.created("Sample graph");
      reload();
    } catch (e) {
      setSampleError(e);
    } finally {
      setBusy(false);
    }
  }
  const empty = !!data && data.nodes === 0 && data.edges === 0;
  return (
    <div className="space-y-4">
      <ErrorView error={error ?? sampleError} onRetry={reload} />
      {loading && !error && <SkeletonLines lines={4} label="Loading graph" />}
      {empty && (
        <EmptyState
          title="Your graph is empty"
          why="A graph stores nodes (things, like people or products) and edges (relationships between them, like “knows” or “bought”). Walk the links to find paths, neighbours and recommendations."
          action={<Button tone="primary" onClick={() => nav("/graph/nodes?new=1")}>New node</Button>}
          sample={<Button disabled={busy} onClick={() => void sample()}>{busy ? "Loading sample…" : "Load sample"}</Button>}
          docs="docs/adr/0006-graph.md"
        />
      )}
      {data && !empty && (
        <>
          <div className="grid gap-3 sm:grid-cols-3">
            <StatTile to="/graph/nodes" label={`${data.nodes} nodes`} title="Nodes" value={data.nodes} action="Browse nodes" />
            <StatTile to="/graph/edges" label={`${data.edges} edges`} title="Edges" value={data.edges} action="Browse edges" />
            <StatTile to="/graph/explorer" label={`Open the explorer: ${data.edge_types.length} edge types`} title="Edge types" value={data.edge_types.length} action="Open explorer" />
          </div>
          <section aria-labelledby="graph-types" className="overflow-hidden rounded-xl border border-border bg-card">
            <header className="flex items-center justify-between gap-2 border-b border-border px-4 py-2.5">
              <h2 id="graph-types" className="text-sm font-medium">Edge types in graph <span className="font-mono">default</span></h2>
            </header>
            {data.edge_types.length === 0 ? (
              <div className="p-4"><EmptyState compact title="No edges yet" why="Connect two nodes with an edge on the Edges tab." /></div>
            ) : (
              <Table aria-label="Edge types" framed density="compact" className="w-full text-left text-sm">
                <Table.Header className="text-xs text-muted-foreground"><Table.Row><Table.Head>Edge type</Table.Head><Table.Head className="w-1/2"><span className="sr-only">Share</span></Table.Head><Table.Head align="end">Count</Table.Head></Table.Row></Table.Header>
                <Table.Body>
                  {data.edge_types.map((t) => (
                    <Table.Row key={t.type} className="border-t border-border hover:bg-muted/40">
                      <Table.Cell className="font-mono">{t.type}</Table.Cell>
                      <Table.Cell><span aria-hidden="true" className="block h-1.5 overflow-hidden rounded-full bg-muted"><span className="block h-full rounded-full bg-primary/70" style={{ width: `${data.edges > 0 ? Math.max(2, Math.round((t.count / data.edges) * 100)) : 0}%` }} /></span></Table.Cell>
                      <Table.Cell align="end">{t.count}</Table.Cell>
                    </Table.Row>
                  ))}
                </Table.Body>
              </Table>
            )}
          </section>
        </>
      )}
      {data && <GraphRules edgeTypes={data.edge_types.map((t) => t.type)} />}
    </div>
  );
}

/** A big figure that links to where it can be browsed. */
function StatTile({ to, label, title, value, action }: { to: string; label: string; title: string; value: number; action: string }) {
  return (
    <Link to={to} aria-label={label} className="group rounded-xl border border-border bg-card px-4 py-3 transition-colors hover:border-primary/50 hover:bg-muted/40 focus-visible:outline-2 focus-visible:outline-ring">
      <span className="block text-xs text-muted-foreground">{title}</span>
      <span className="mt-0.5 block text-2xl font-semibold tabular-nums tracking-tight">{value.toLocaleString("en-US")}</span>
      <span className="mt-1 block text-xs font-medium text-primary group-hover:underline">{action} →</span>
    </Link>
  );
}

/** Rules for a node label or an edge type (FB-139). Pick the resource, then edit its rules. */
function GraphRules({ edgeTypes }: { edgeTypes: string[] }) {
  const [kind, setKind] = useState<"graph_label" | "graph_edge_type">("graph_label");
  const [name, setName] = useState("");
  const [applied, setApplied] = useState("");
  return (
    <section aria-label="Graph access rules" className="space-y-3 rounded-xl border border-border bg-card p-4">
      <h2 className="text-sm font-medium">Access rules</h2>
      <p className="max-w-3xl text-sm text-muted-foreground">
        Rules decide who may read or change nodes of a label, or edges of a type. They can test <code className="font-mono">properties.&lt;key&gt;</code>, such as <code className="font-mono">properties.owner = @request.auth.id</code>. An edge is visible only when its type is allowed and both of its nodes are visible. Use the name <code className="font-mono">*</code> for every label or type without rules of its own.
      </p>
      <form className="flex flex-wrap items-start gap-3" onSubmit={(e) => { e.preventDefault(); setApplied(name.trim()); }}>
        <FormField label="Rules for">{(p) => <Sel {...p} aria-label="Rules for" value={kind} onChange={(v) => { setKind(v as typeof kind); setApplied(""); }} options={[{ value: "graph_label", label: "A node label" }, { value: "graph_edge_type", label: "An edge type" }]} />}</FormField>
        <FormField label={kind === "graph_label" ? "Label" : "Edge type"} help="Type a name, or * for all.">
          {(p) => <Input {...p} size="sm" list="graph-rule-names" className="w-48" value={name} onChange={(e) => setName(e.target.value)} />}
        </FormField>
        <datalist id="graph-rule-names">{(kind === "graph_edge_type" ? edgeTypes : []).map((t) => <option key={t} value={t} />)}</datalist>
        <div className="pt-6"><Button type="submit" disabled={!name.trim()}>Show rules</Button></div>
      </form>
      {applied ? <ResourceRules kind={kind} name={applied} noun={kind === "graph_label" ? "label" : "edge type"} /> : null}
    </section>
  );
}
