import { useState } from "react";
import { shortestPath, traverse } from "../../api";
import type { PathQuery, PathResult, TraverseQuery, TraverseResult } from "../../api";
import { Button, ErrorView, Panel, inputCls } from "../../components/ui";

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
            <button key={k} type="button" role="tab" aria-selected={kind === k} onClick={() => pick(k)} className={`rounded-md border px-3 py-1 text-sm ${kind === k ? "border-amber-500 text-amber-300" : "border-zinc-700 text-zinc-400"}`}>
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
          <table className="w-full text-left text-sm">
            <thead className="text-xs uppercase text-zinc-500"><tr><th className="py-1">Depth</th><th>Id</th><th>Label</th></tr></thead>
            <tbody>{result.nodes.map((n) => <tr key={n.node.id} className="border-t border-zinc-800"><td className="py-1">{n.depth}</td><td className="font-mono">{n.node.id}</td><td>{n.node.label}</td></tr>)}</tbody>
          </table>
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
