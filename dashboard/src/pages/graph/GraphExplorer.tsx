import { useEffect, useState } from "react";
import { useSearchParams } from "react-router-dom";
import { getNode, neighbors, shortestPath, traverse } from "../../api";
import type { Direction, GraphEdge, GraphNode, Neighbor, Page } from "../../api";
import { Button, ErrorView, Panel } from "../../components/ui";
import { layout, type Pos } from "./layout";
import { Input } from "panelui-web/input";
import { Sel } from "../../components/controls";

const W = 800;
const H = 480;
// Theme tokens, so nodes follow light/dark and the accent (chart-1 is the accent).
const PALETTE = ["var(--chart-1)", "var(--chart-2)", "var(--chart-3)", "var(--chart-4)", "var(--chart-5)", "var(--info)", "var(--success)"];
const colour = (label: string) => PALETTE[[...label].reduce((a, c) => a + c.charCodeAt(0), 0) % PALETTE.length]!;

interface View { nodes: Map<string, GraphNode>; edges: Map<string, GraphEdge>; pos: Map<string, Pos> }
const EMPTY: View = { nodes: new Map(), edges: new Map(), pos: new Map() };

function merge(view: View, nodes: GraphNode[], edges: GraphEdge[]): View {
  const n = new Map(view.nodes), e = new Map(view.edges);
  nodes.forEach((x) => n.set(x.id, x));
  // An edge is drawn only when both ends are on screen.
  edges.forEach((x) => e.set(x.id, x));
  const live = [...e.values()].filter((x) => n.has(x.source) && n.has(x.target));
  const edgeMap = new Map(live.map((x) => [x.id, x]));
  return { nodes: n, edges: edgeMap, pos: layout([...n.keys()], live.map((x) => [x.source, x.target]), view.pos, W, H) };
}

interface StartData { id: string; node: GraphNode; nb: Page<Neighbor> }
async function fetchStart(id: string): Promise<StartData> {
  const node = await getNode(id);
  return { id, node, nb: await neighbors(id, { direction: "both", limit: 100 }) };
}

/** Interactive explorer: expand neighbours, traversal with depth, shortest path between two nodes. */
export default function GraphExplorer() {
  const [params] = useSearchParams();
  const seed = params.get("node") ?? "";
  const [start, setStart] = useState(seed);
  const [view, setView] = useState<View>(EMPTY);
  const [selected, setSelected] = useState<string>();
  const [path, setPath] = useState<{ nodes: Set<string>; edges: Set<string> }>();
  const [depth, setDepth] = useState("2");
  const [direction, setDirection] = useState<Direction>("both");
  const [pathTo, setPathTo] = useState("");
  const [status, setStatus] = useState<string>();
  const [error, setError] = useState<unknown>();

  async function guard(fn: () => Promise<void>) {
    setError(undefined);
    setStatus(undefined);
    try {
      await fn();
    } catch (e) {
      setError(e);
    }
  }

  const applyStart = ({ node, nb, id }: StartData) => {
    setView(merge(merge(EMPTY, [node], []), nb.items.map((x) => x.node), nb.items.map((x) => x.edge)));
    setSelected(node.id);
    setPath(undefined);
    setStatus(`Loaded ${id} and ${nb.items.length} neighbours`);
  };
  const loadStart = (id: string) => guard(async () => applyStart(await fetchStart(id)));

  useEffect(() => {
    if (!seed) return;
    let live = true;
    fetchStart(seed).then(
      (d) => live && applyStart(d),
      (e: unknown) => live && setError(e),
    );
    return () => {
      live = false;
    };
  }, [seed]);

  const expand = () =>
    guard(async () => {
      if (!selected) throw new Error("Select a node first");
      const nb = await neighbors(selected, { direction, limit: 100 });
      setView((v) => merge(v, nb.items.map((x) => x.node), nb.items.map((x) => x.edge)));
      setStatus(`Expanded ${selected}: ${nb.items.length} neighbours${nb.next_after ? " (more available)" : ""}`);
    });

  const runTraverse = () =>
    guard(async () => {
      if (!selected) throw new Error("Select a node first");
      const r = await traverse({ start: selected, direction, max_depth: Number(depth), include_start: true, include_edges: true, limit: 200 });
      setView((v) => merge(v, r.nodes.map((x) => x.node), r.edges));
      setStatus(`Traversal to depth ${depth}: ${r.nodes.length} nodes, ${r.edges.length} edges${r.truncated ? " (truncated)" : ""}`);
    });

  const findPath = () =>
    guard(async () => {
      if (!selected) throw new Error("Select a node first");
      if (!pathTo.trim()) throw new Error("Enter the id of the target node");
      const r = await shortestPath({ from: selected, to: pathTo.trim(), direction });
      if (!r.found) {
        setPath(undefined);
        setStatus(`No path from ${selected} to ${pathTo}`);
        return;
      }
      setView((v) => merge(v, r.nodes, r.edges));
      setPath({ nodes: new Set(r.nodes.map((n) => n.id)), edges: new Set(r.edges.map((e) => e.id)) });
      setStatus(`Path of length ${r.length}: ${r.nodes.map((n) => n.id).join(" → ")}`);
    });

  const sel = selected ? view.nodes.get(selected) : undefined;
  return (
    <div className="space-y-4">
      <Panel title="Explorer controls">
        <div className="flex flex-wrap items-end gap-2">
          <label className="text-sm">Start node id<Input size="sm" aria-label="Start node id" className="mt-1 block" value={start} onChange={(e) => setStart(e.target.value)} /></label>
          <Button tone="primary" onClick={() => void loadStart(start.trim())} disabled={!start.trim()}>Load node</Button>
          <label className="text-sm">Direction
            <Sel aria-label="Direction" className="mt-1" value={direction} onChange={(v) => setDirection(v as Direction)} options={[{ value: "both", label: "both" }, { value: "out", label: "out" }, { value: "in", label: "in" }]} />
          </label>
          <Button onClick={() => void expand()}>Expand neighbours</Button>
          <label className="text-sm">Depth<Input size="sm" aria-label="Depth" type="number" min={1} max={10} className="mt-1 block w-20" value={depth} onChange={(e) => setDepth(e.target.value)} /></label>
          <Button onClick={() => void runTraverse()}>Traverse</Button>
          <label className="text-sm">Path to<Input size="sm" aria-label="Path target id" className="mt-1 block" value={pathTo} onChange={(e) => setPathTo(e.target.value)} /></label>
          <Button onClick={() => void findPath()}>Find path</Button>
          <Button onClick={() => { setView(EMPTY); setSelected(undefined); setPath(undefined); setStatus(undefined); }}>Clear</Button>
        </div>
        <p className="mt-2 text-xs text-muted-foreground">Select a node in the canvas, then expand, traverse from it or find the shortest path to another node id.</p>
      </Panel>
      <ErrorView error={error} />
      {status && <p role="status" className="text-sm text-success">{status}</p>}
      <div className="grid gap-4 lg:grid-cols-[1fr_18rem]">
        <svg role="img" aria-label="Graph canvas" viewBox={`0 0 ${W} ${H}`} className="w-full rounded-lg border border-border bg-background">
          <defs>
            <marker id="arrow" viewBox="0 0 10 10" refX="24" refY="5" markerWidth="6" markerHeight="6" orient="auto-start-reverse"><path d="M0 0L10 5L0 10z" style={{ fill: "var(--muted-foreground)" }} /></marker>
          </defs>
          {[...view.edges.values()].map((e) => {
            const a = view.pos.get(e.source), b = view.pos.get(e.target);
            if (!a || !b) return null;
            const on = path?.edges.has(e.id);
            return (
              <g key={e.id} data-testid={`edge-${e.id}`} data-on-path={on ? "true" : undefined}>
                <line x1={a.x} y1={a.y} x2={b.x} y2={b.y} style={{ stroke: on ? "var(--primary)" : "var(--muted-foreground)" }} strokeOpacity={on ? 1 : 0.55} strokeWidth={on ? 3 : 1.5} markerEnd={e.directed ? "url(#arrow)" : undefined} />
                <text x={(a.x + b.x) / 2} y={(a.y + b.y) / 2 - 4} fontSize="9" style={{ fill: "var(--muted-foreground)" }} textAnchor="middle">{e.type}</text>
              </g>
            );
          })}
          {[...view.nodes.values()].map((n) => {
            const p = view.pos.get(n.id);
            if (!p) return null;
            const on = path?.nodes.has(n.id);
            return (
              <g key={n.id} role="button" tabIndex={0} aria-label={`Node ${n.id}`} aria-pressed={selected === n.id} data-on-path={on ? "true" : undefined}
                onClick={() => setSelected(n.id)} onKeyDown={(e) => { if (e.key === "Enter" || e.key === " ") setSelected(n.id); }} className="cursor-pointer">
                <circle cx={p.x} cy={p.y} r={14} style={{ fill: colour(n.label), stroke: selected === n.id ? "var(--foreground)" : on ? "var(--primary)" : "var(--background)" }} strokeWidth={selected === n.id || on ? 3 : 1.5} />
                <text x={p.x} y={p.y + 28} fontSize="10" style={{ fill: "var(--foreground)" }} textAnchor="middle">{n.id}</text>
              </g>
            );
          })}
        </svg>
        <Panel title={sel ? `Node ${sel.id}` : "Details"}>
          {sel ? (
            <div className="text-sm">
              <p className="text-muted-foreground">Label: {sel.label || "none"}</p>
              <pre aria-label="Node properties" className="mt-2 max-h-72 overflow-auto text-xs text-foreground/80">{JSON.stringify(sel.properties, null, 2)}</pre>
            </div>
          ) : (
            <p className="text-sm text-muted-foreground">{view.nodes.size === 0 ? "Load a node to begin." : "Select a node."}</p>
          )}
          <p className="mt-3 text-xs text-muted-foreground">{view.nodes.size} nodes · {view.edges.size} edges on screen</p>
        </Panel>
      </div>
    </div>
  );
}
