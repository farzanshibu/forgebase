import { useEffect, useState } from "react";
import { useSearchParams } from "react-router-dom";
import { getNode, neighbors, shortestPath, traverse } from "../../api";
import type { Direction, GraphEdge, GraphNode, Neighbor, Page } from "../../api";
import { Button, ErrorView } from "../../components/ui";
import { FormField } from "../../components/FormField";
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
      <section aria-label="Explorer controls" className="space-y-3 rounded-xl border border-border bg-card p-4">
        <div className="flex flex-wrap items-end gap-x-6 gap-y-3">
          <div className="flex items-end gap-2">
            <FormField label="Start node id">{(p) => <Input {...p} size="sm" aria-label="Start node id" className="w-48 font-mono" placeholder="alice" value={start} onChange={(e) => setStart(e.target.value)} onKeyDown={(e) => { if (e.key === "Enter" && start.trim()) void loadStart(start.trim()); }} />}</FormField>
            <Button tone="primary" onClick={() => void loadStart(start.trim())} disabled={!start.trim()}>Load node</Button>
          </div>
          <span aria-hidden="true" className="hidden h-8 w-px bg-border md:block" />
          <div className="flex flex-wrap items-end gap-2">
            <FormField label="Direction">{(p) => <Sel {...p} aria-label="Direction" className="w-24" value={direction} onChange={(v) => setDirection(v as Direction)} options={[{ value: "both", label: "both" }, { value: "out", label: "out" }, { value: "in", label: "in" }]} />}</FormField>
            <Button onClick={() => void expand()} disabled={!selected}>Expand neighbours</Button>
          </div>
          <div className="flex items-end gap-2">
            <FormField label="Depth">{(p) => <Input {...p} size="sm" aria-label="Depth" type="number" min={1} max={10} className="w-16 tabular-nums" value={depth} onChange={(e) => setDepth(e.target.value)} />}</FormField>
            <Button onClick={() => void runTraverse()} disabled={!selected}>Traverse</Button>
          </div>
          <div className="flex items-end gap-2">
            <FormField label="Path to">{(p) => <Input {...p} size="sm" aria-label="Path target id" className="w-36 font-mono" placeholder="node id" value={pathTo} onChange={(e) => setPathTo(e.target.value)} />}</FormField>
            <Button onClick={() => void findPath()} disabled={!selected}>Find path</Button>
          </div>
          <Button className="ml-auto" onClick={() => { setView(EMPTY); setSelected(undefined); setPath(undefined); setStatus(undefined); }} disabled={view.nodes.size === 0}>Clear</Button>
        </div>
        <p className="text-xs text-muted-foreground">{selected ? <>Selected <span className="font-mono text-foreground">{selected}</span>. Expand, traverse from it, or find the shortest path to another node.</> : "Load a node, then select one in the canvas to expand, traverse from it or find the shortest path to another node."}</p>
      </section>
      <ErrorView error={error} />
      {status && <p role="status" className="rounded-lg border border-success/40 bg-success/5 px-3 py-2 text-sm text-success">{status}</p>}
      <div className="grid items-start gap-4 lg:grid-cols-[minmax(0,1fr)_18rem]">
        <svg role="img" aria-label="Graph canvas" viewBox={`0 0 ${W} ${H}`} className="w-full rounded-xl border border-border bg-card [background-image:radial-gradient(var(--border)_1px,transparent_1px)] [background-size:20px_20px]">
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
        <section aria-label="Node details" className="overflow-hidden rounded-xl border border-border bg-card">
          <header className="border-b border-border px-4 py-2.5">
            <h2 className="truncate text-sm font-medium" title={sel?.id}>{sel ? <>Node <span className="font-mono">{sel.id}</span></> : "Details"}</h2>
          </header>
          <div className="px-4 py-3">
            {sel ? (
              <div className="space-y-2 text-sm">
                <p className="flex items-center gap-2 text-muted-foreground">Label {sel.label ? <span className="rounded border border-border bg-muted/40 px-1.5 font-mono text-xs leading-5 text-foreground">{sel.label}</span> : "none"}</p>
                <pre aria-label="Node properties" className="max-h-72 overflow-auto rounded-lg border border-border bg-muted/40 p-2 font-mono text-xs leading-5 text-foreground">{JSON.stringify(sel.properties, null, 2)}</pre>
              </div>
            ) : (
              <p className="text-sm text-muted-foreground">{view.nodes.size === 0 ? "Load a node to begin." : "Select a node."}</p>
            )}
          </div>
          <p className="border-t border-border px-4 py-2 text-xs tabular-nums text-muted-foreground">{view.nodes.size} nodes · {view.edges.size} edges on screen</p>
        </section>
      </div>
    </div>
  );
}
