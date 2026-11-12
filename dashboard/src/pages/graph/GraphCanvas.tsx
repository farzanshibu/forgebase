/**
 * GraphCanvas: the explorer's picture of the graph, drawn with PanelUI's Flow (its node graph). Each node is a card
 * with its id and label; edges follow the cards; the canvas pans by dragging, zooms with a pinch or Ctrl/⌘ + wheel,
 * and has zoom and fit buttons. Nodes are dragged to arrange them and reached with Tab (arrow keys move one,
 * Enter selects it).
 *
 * Usage:
 *   <GraphCanvas nodes={nodes} edges={edges} pos={centres} selected={id} path={path} fitKey={n}
 *     onSelect={setSelected} onMove={(id, centre) => …} onExpand={(id) => …} />
 *
 * `pos` holds each node's centre (what layout.ts works in). `fitKey` is a number the caller bumps when the set of
 * nodes changes: the canvas then fits everything into view again.
 *
 * Flow has no edge labels, so the edge type is a small pill at the middle of the edge, placed from the two node
 * centres. Flow keeps a node's position to itself while it is dragged, so the pills of that node are hidden for the
 * drag and come back in the right place on release.
 */
import { Flow } from "panelui-web/flow";
import { useRef, useState, type PointerEvent } from "react";
import type { GraphEdge, GraphNode } from "../../api";
import type { Pos } from "./layout";

/** A node card's box. Fixed, so an edge label can be placed from the node centres alone. */
export const NODE_W = 168;
export const NODE_H = 56;

// Theme tokens, so nodes follow light/dark and the accent (chart-1 is the accent).
const PALETTE = ["var(--chart-1)", "var(--chart-2)", "var(--chart-3)", "var(--chart-4)", "var(--chart-5)", "var(--info)", "var(--success)"];
const colour = (label: string) => PALETTE[[...label].reduce((a, c) => a + c.charCodeAt(0), 0) % PALETTE.length]!;

export interface GraphCanvasProps {
  nodes: GraphNode[];
  edges: GraphEdge[];
  /** Node centres. */
  pos: Map<string, Pos>;
  selected?: string;
  /** The nodes and edges of a found path, drawn in the accent colour. */
  path?: { nodes: Set<string>; edges: Set<string> };
  /** Bump it when the set of nodes changes: the canvas fits them into view again. */
  fitKey: number;
  onSelect: (id: string) => void;
  /** A node was dragged (or moved with the arrow keys) to a new centre. */
  onMove: (id: string, centre: Pos) => void;
  /** A node was double-clicked. */
  onExpand: (id: string) => void;
}

export function GraphCanvas({ nodes, edges, pos, selected, path, fitKey, onSelect, onMove, onExpand }: GraphCanvasProps) {
  // The node being dragged: its edge labels are hidden until it is dropped (see the file comment).
  const [dragging, setDragging] = useState<string>();
  const press = useRef<{ id: string; x: number; y: number } | null>(null);
  const onDown = (e: PointerEvent) => {
    const id = (e.target as HTMLElement).closest<HTMLElement>("[data-flow-node]")?.dataset.flowNode;
    press.current = id ? { id, x: e.clientX, y: e.clientY } : null;
  };
  const onMoveCapture = (e: PointerEvent) => {
    const p = press.current;
    if (p && dragging !== p.id && Math.hypot(e.clientX - p.x, e.clientY - p.y) >= 3) setDragging(p.id);
  };
  const onUp = () => {
    press.current = null;
    setDragging(undefined);
  };

  // Several edges between the same two nodes share a line: their labels are stacked, not drawn on top of each other.
  const lane = new Map<string, number>();
  const labels = edges.map((e) => {
    const a = pos.get(e.source), b = pos.get(e.target);
    if (!a || !b) return null;
    const pair = [e.source, e.target].sort().join("\u0000");
    const i = lane.get(pair) ?? 0;
    lane.set(pair, i + 1);
    const loop = e.source === e.target;
    const on = path?.edges.has(e.id);
    const hidden = dragging === e.source || dragging === e.target;
    return (
      <span
        key={e.id}
        data-testid={`edge-${e.id}`}
        data-on-path={on ? "true" : undefined}
        className={`pointer-events-none absolute z-[5] -translate-x-1/2 -translate-y-1/2 rounded-full border px-1.5 py-px font-mono text-xs whitespace-nowrap transition-opacity duration-150 ${on ? "border-primary bg-primary text-primary-foreground" : "border-border bg-card text-muted-foreground"} ${hidden ? "opacity-0" : ""}`.trim()}
        // A loop has no line to sit on: its label goes just above the node.
        style={{ left: (a.x + b.x) / 2, top: (a.y + b.y) / 2 + (loop ? -(NODE_H / 2 + 14) : 0) + i * 20 }}
      >
        {e.type}
      </span>
    );
  });

  return (
    <div className="relative min-w-0" onPointerDownCapture={onDown} onPointerMoveCapture={onMoveCapture} onPointerUpCapture={onUp} onPointerCancelCapture={onUp}>
      <Flow
        key={fitKey}
        label="Graph canvas"
        fitViewOnMount
        fitViewPadding={56}
        minZoom={0.15}
        className="h-[30rem] rounded-xl border-border bg-card"
        onNodeDragEnd={(id, p) => onMove(id, { x: p.x + NODE_W / 2, y: p.y + NODE_H / 2 })}
      >
        <Flow.Background variant="dots" gap={20} />
        {edges.map((e) => {
          if (e.source === e.target || !pos.has(e.source) || !pos.has(e.target)) return null;
          const on = path?.edges.has(e.id);
          return <Flow.Edge key={e.id} from={{ node: e.source }} to={{ node: e.target }} arrow={e.directed} color={on ? "primary" : undefined} width={on ? 3 : 1.5} />;
        })}
        {labels}
        {nodes.map((n) => {
          const c = pos.get(n.id);
          if (!c) return null;
          const on = path?.nodes.has(n.id);
          const isSelected = selected === n.id;
          return (
            <Flow.Node
              key={n.id}
              id={n.id}
              position={{ x: c.x - NODE_W / 2, y: c.y - NODE_H / 2 }}
              selected={isSelected}
              // A toggle button to assistive tech (Flow would mark it aria-selected, which a button cannot carry).
              role="button"
              aria-label={`Node ${n.id}`}
              aria-pressed={isSelected}
              aria-selected={undefined}
              data-on-path={on ? "true" : undefined}
              onPress={() => onSelect(n.id)}
              onDoubleClick={() => onExpand(n.id)}
              className={`flex items-center gap-2.5 rounded-xl px-3 ${on ? "border-primary bg-[color-mix(in_oklab,var(--primary)_10%,var(--card))]" : "border-border"}`}
              style={{ width: NODE_W, height: NODE_H }}
            >
              <span aria-hidden="true" className="size-2.5 shrink-0 rounded-full ring-2 ring-background" style={{ background: colour(n.label) }} />
              <span className="min-w-0">
                <span className="block truncate font-mono text-sm font-medium" title={n.id}>{n.id}</span>
                <span className="block truncate text-xs text-muted-foreground">{n.label || "No label"}</span>
              </span>
            </Flow.Node>
          );
        })}
        {nodes.length > 0 && <Flow.Controls lock={false} />}
        {nodes.length > 12 && <Flow.MiniMap />}
      </Flow>
      {nodes.length === 0 && (
        <div className="pointer-events-none absolute inset-0 flex flex-col items-center justify-center gap-1 px-6 text-center">
          <p className="text-sm font-medium">Nothing on the canvas yet</p>
          <p className="max-w-sm text-sm text-muted-foreground">Enter a node id above and choose Load node. It appears here with its neighbours.</p>
        </div>
      )}
    </div>
  );
}
