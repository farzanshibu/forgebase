import { useState } from "react";
import { shortestPath, traverse } from "../../api";
import type { PathQuery, PathResult, TraverseQuery, TraverseResult } from "../../api";
import { Tabs } from "panelui-web/tabs";
import { Button, ErrorView } from "../../components/ui";
import { Table } from "panelui-web/table";
import { Textarea } from "panelui-web/textarea";

type Kind = "traverse" | "path";
const EXAMPLES: Record<Kind, string> = {
  traverse: JSON.stringify({ start: "node-id", direction: "out", edge_types: [], max_depth: 3, limit: 100, include_start: true }, null, 2),
  path: JSON.stringify({ from: "node-a", to: "node-b", direction: "both", max_depth: 6 }, null, 2),
};

/** Runs a traversal or shortest-path query written as the JSON body the API takes. */
export default function QueryRunner() {
  const [kind, setKind] = useState<Kind>("traverse");
  const [text, setText] = useState(EXAMPLES.traverse);
  const [error, setError] = useState<unknown>();
  const [result, setResult] = useState<TraverseResult | PathResult>();

  function pick(k: Kind) {
    setKind(k);
    setText(EXAMPLES[k]);
    setResult(undefined);
    setError(undefined);
  }

  async function run() {
    setError(undefined);
    setResult(undefined);
    let body: unknown;
    try {
      body = JSON.parse(text);
    } catch (e) {
      setError(new Error(`Invalid JSON: ${(e as Error).message}`));
      return;
    }
    try {
      setResult(kind === "traverse" ? await traverse(body as TraverseQuery) : await shortestPath(body as PathQuery));
    } catch (e) {
      setError(e);
    }
  }

  const card = "overflow-hidden rounded-xl border border-border bg-card";
  const head = "flex flex-wrap items-center justify-between gap-2 border-b border-border px-4 py-2.5";
  return (
    <div className="space-y-4">
      <section aria-labelledby="graph-qr" className={card}>
        <header className={head}>
          <h2 id="graph-qr" className="text-sm font-medium">Query runner</h2>
          <Tabs variant="segmented" value={kind} onValueChange={(v) => pick(v as Kind)}>
            <Tabs.List aria-label="Query type">
              <Tabs.Trigger value="traverse">Traversal</Tabs.Trigger>
              <Tabs.Trigger value="path">Shortest path</Tabs.Trigger>
            </Tabs.List>
          </Tabs>
        </header>
        <div className="space-y-3 p-4">
          <p className="text-xs text-muted-foreground">The JSON body the API takes: <code className="font-mono">POST /api/v1/graph/{kind === "traverse" ? "traverse" : "shortest-path"}</code>. <kbd className="rounded border border-border bg-background px-1 font-mono">Ctrl/Cmd+Enter</kbd> runs it.</p>
          <Textarea size="sm" autoResize={false} aria-label="Query JSON" spellCheck={false} className="h-48 w-full font-mono text-xs leading-5" value={text} onChange={(e) => setText(e.target.value)}
            onKeyDown={(e) => { if ((e.ctrlKey || e.metaKey) && e.key === "Enter") { e.preventDefault(); void run(); } }} />
          <div className="flex items-center gap-2">
            <Button tone="primary" onClick={() => void run()}>Run query</Button>
            <Button onClick={() => pick(kind)}>Reset example</Button>
          </div>
        </div>
      </section>
      <ErrorView error={error} />
      {result && "start" in result && (
        <section aria-labelledby="graph-qr-result" className={card}>
          <header className={head}><h2 id="graph-qr-result" className="text-sm font-medium">{`${result.nodes.length} nodes, ${result.edges.length} edges${result.truncated ? " (truncated)" : ""}`}</h2></header>
          <Table framed density="compact" className="w-full text-left text-sm">
            <Table.Header className="text-xs text-muted-foreground"><Table.Row><Table.Head align="end" className="w-20">Depth</Table.Head><Table.Head>Id</Table.Head><Table.Head>Label</Table.Head></Table.Row></Table.Header>
            <Table.Body>{result.nodes.map((n) => <Table.Row key={n.node.id} className="border-t border-border hover:bg-muted/40"><Table.Cell align="end">{n.depth}</Table.Cell><Table.Cell className="font-mono">{n.node.id}</Table.Cell><Table.Cell>{n.node.label ? <span className="rounded border border-border bg-muted/40 px-1.5 font-mono text-xs leading-5">{n.node.label}</span> : <span className="text-muted-foreground">none</span>}</Table.Cell></Table.Row>)}</Table.Body>
          </Table>
        </section>
      )}
      {result && "found" in result && (
        <section aria-labelledby="graph-qr-path" className={card}>
          <header className={head}><h2 id="graph-qr-path" className="text-sm font-medium">{result.found ? `Path of length ${result.length}` : "No path found"}</h2></header>
          <div className="p-4">
            {result.found ? <p className="font-mono text-sm">{result.nodes.map((n) => n.id).join(" → ")}</p> : <p className="text-sm text-muted-foreground">The two nodes are not connected within the depth you gave. Raise max_depth or try direction both.</p>}
          </div>
        </section>
      )}
    </div>
  );
}
