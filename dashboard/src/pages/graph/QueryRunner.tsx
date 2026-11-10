import { useState } from "react";
import { shortestPath, traverse } from "../../api";
import type { PathQuery, PathResult, TraverseQuery, TraverseResult } from "../../api";
import { Button, ErrorView, Panel, inputCls } from "../../components/ui";
import { Table } from "panelui-web/table";

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

  return (
    <div className="space-y-4">
      <Panel title="Query runner">
        <div role="tablist" aria-label="Query type" className="mb-3 flex gap-2">
          {(["traverse", "path"] as const).map((k) => (
            <button key={k} type="button" role="tab" aria-selected={kind === k} onClick={() => pick(k)} className={`rounded-md border px-3 py-1 text-sm ${kind === k ? "border-primary text-warning" : "border-border text-muted-foreground"}`}>
              {k === "traverse" ? "Traversal" : "Shortest path"}
            </button>
          ))}
        </div>
        <label className="block text-sm">Query (JSON)
          <textarea aria-label="Query JSON" className={`${inputCls} mt-1 h-44 w-full font-mono`} value={text} onChange={(e) => setText(e.target.value)} />
        </label>
        <div className="mt-3"><Button tone="primary" onClick={() => void run()}>Run query</Button></div>
      </Panel>
      <ErrorView error={error} />
      {result && "start" in result && (
        <Panel title={`${result.nodes.length} nodes, ${result.edges.length} edges${result.truncated ? " (truncated)" : ""}`}>
          <Table className="w-full text-left text-sm">
            <Table.Header className="text-xs uppercase text-muted-foreground"><Table.Row><Table.Head className="py-1">Depth</Table.Head><Table.Head>Id</Table.Head><Table.Head>Label</Table.Head></Table.Row></Table.Header>
            <Table.Body>{result.nodes.map((n) => <Table.Row key={n.node.id} className="border-t border-border"><Table.Cell className="py-1">{n.depth}</Table.Cell><Table.Cell className="font-mono">{n.node.id}</Table.Cell><Table.Cell>{n.node.label}</Table.Cell></Table.Row>)}</Table.Body>
          </Table>
        </Panel>
      )}
      {result && "found" in result && (
        <Panel title={result.found ? `Path of length ${result.length}` : "No path found"}>
          {result.found && <p className="font-mono text-sm">{result.nodes.map((n) => n.id).join(" → ")}</p>}
        </Panel>
      )}
    </div>
  );
}
