import { Link } from "react-router-dom";
import type { JobStatus, WorkflowStep } from "../../api";

const W = 168, H = 46, GX = 56, GY = 18, PAD = 12;
const FILL: Record<JobStatus, string> = { queued: "#3f3f46", running: "#92400e", succeeded: "#065f46", dead: "#7f1d1d", cancelled: "#52525b" };

export interface Placed { id: string; label: string; status: JobStatus; waiting: number; level: number; row: number; compensation: boolean; x: number; y: number }

/** Layered layout: a step sits one column right of its deepest parent; rows follow the order in the list. */
export function layout(steps: WorkflowStep[]): { nodes: Placed[]; edges: { from: Placed; to: Placed }[]; width: number; height: number } {
  const byId = new Map(steps.map((s) => [s.job.id, s]));
  const level = new Map<string, number>();
  const depth = (id: string, seen: Set<string>): number => {
    const known = level.get(id);
    if (known !== undefined) return known;
    if (seen.has(id)) return 0; // a cycle cannot come from the server; stay safe
    seen.add(id);
    const s = byId.get(id);
    const d = s ? Math.max(-1, ...s.depends_on.filter((p) => byId.has(p)).map((p) => depth(p, seen))) + 1 : 0;
    level.set(id, d);
    return d;
  };
  for (const s of steps) depth(s.job.id, new Set());
  const rows = new Map<number, number>();
  const nodes: Placed[] = steps.map((s) => {
    const lv = level.get(s.job.id) ?? 0;
    const row = rows.get(lv) ?? 0;
    rows.set(lv, row + 1);
    return {
      id: s.job.id, label: s.name ?? s.job.kind, status: s.job.status, waiting: s.job.waiting_for ?? 0, level: lv, row, compensation: s.role === "compensation",
      x: PAD + lv * (W + GX), y: PAD + row * (H + GY),
    };
  });
  const at = new Map(nodes.map((n) => [n.id, n]));
  const edges = steps.flatMap((s) => s.depends_on.flatMap((p) => (at.has(p) ? [{ from: at.get(p)!, to: at.get(s.job.id)! }] : [])));
  const cols = Math.max(0, ...nodes.map((n) => n.level)) + 1;
  const maxRows = Math.max(1, ...rows.values());
  return { nodes, edges, width: PAD * 2 + cols * W + (cols - 1) * GX, height: PAD * 2 + maxRows * H + (maxRows - 1) * GY };
}

/** The steps of a workflow as a left-to-right graph; each node links to its job. */
export default function WorkflowGraph({ steps }: { steps: WorkflowStep[] }) {
  if (steps.length === 0) return <p className="text-sm text-zinc-400">This workflow has no steps.</p>;
  const g = layout(steps);
  return (
    <div className="overflow-x-auto">
      <svg role="img" aria-label="Workflow graph" width={g.width} height={g.height} viewBox={`0 0 ${g.width} ${g.height}`} className="text-zinc-100">
        {g.edges.map((e) => {
          const x1 = e.from.x + W, y1 = e.from.y + H / 2, x2 = e.to.x, y2 = e.to.y + H / 2, mx = (x1 + x2) / 2;
          return <path key={`${e.from.id}>${e.to.id}`} data-edge={`${e.from.id}>${e.to.id}`} d={`M${x1},${y1} C${mx},${y1} ${mx},${y2} ${x2},${y2}`} fill="none" stroke="#71717a" strokeWidth={1.5} />;
        })}
        {g.nodes.map((n) => (
          <Link key={n.id} to={`/jobs/j/${encodeURIComponent(n.id)}`} aria-label={`${n.label}: ${n.status}${n.waiting > 0 ? `, waiting for ${n.waiting}` : ""}`}>
            <g data-status={n.status}>
              <rect x={n.x} y={n.y} width={W} height={H} rx={6} fill={FILL[n.status]} stroke={n.compensation ? "#fbbf24" : "#a1a1aa"} strokeDasharray={n.compensation ? "4 3" : undefined} />
              <text x={n.x + 10} y={n.y + 19} fontSize={13} fill="currentColor">{n.label.length > 22 ? `${n.label.slice(0, 21)}…` : n.label}</text>
              <text x={n.x + 10} y={n.y + 36} fontSize={11} fill="#d4d4d8">{n.status}{n.waiting > 0 ? ` · waiting ${n.waiting}` : ""}{n.compensation ? " · compensation" : ""}</text>
            </g>
          </Link>
        ))}
      </svg>
    </div>
  );
}
