import { Link } from "react-router-dom";
import { graphStats } from "../../api";
import { ErrorView, Loading, Panel } from "../../components/ui";
import { useLoad } from "../../hooks";

/** The API serves one graph per tenant (ADR-0006), so the graph list is that graph with its statistics. */
export default function GraphOverview() {
  const { data, error, loading } = useLoad(graphStats, "graph-stats");
  return (
    <div className="space-y-4">
      <ErrorView error={error} />
      {loading && !error && <Loading what="Loading graph" />}
      {data && (
        <Panel title="Graph: default">
          <p className="text-sm">
            <Link className="text-amber-300 hover:underline" to="/graph/nodes">{data.nodes} nodes</Link> ·{" "}
            <Link className="text-amber-300 hover:underline" to="/graph/edges">{data.edges} edges</Link> ·{" "}
            <Link className="text-amber-300 hover:underline" to="/graph/explorer">Open explorer</Link>
          </p>
          <table className="mt-3 w-full text-left text-sm">
            <thead className="text-xs uppercase text-zinc-500"><tr><th className="py-1">Edge type</th><th>Count</th></tr></thead>
            <tbody>
              {data.edge_types.map((t) => (
                <tr key={t.type} className="border-t border-zinc-800"><td className="py-1 font-mono">{t.type}</td><td>{t.count}</td></tr>
              ))}
              {data.edge_types.length === 0 && <tr><td colSpan={2} className="py-2 text-zinc-400">No edges yet.</td></tr>}
            </tbody>
          </table>
        </Panel>
      )}
    </div>
  );
}
