import { Link } from "react-router-dom";
import { graphStats } from "../../api";
import { ErrorView, Loading, Panel } from "../../components/ui";
import { useLoad } from "../../hooks";
import { Table } from "panelui-web/table";

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
            <Link className="text-primary hover:underline" to="/graph/nodes">{data.nodes} nodes</Link> ·{" "}
            <Link className="text-primary hover:underline" to="/graph/edges">{data.edges} edges</Link> ·{" "}
            <Link className="text-primary hover:underline" to="/graph/explorer">Open explorer</Link>
          </p>
          <Table className="mt-3 w-full text-left text-sm">
            <Table.Header className="text-xs uppercase text-muted-foreground"><Table.Row><Table.Head className="py-1">Edge type</Table.Head><Table.Head>Count</Table.Head></Table.Row></Table.Header>
            <Table.Body>
              {data.edge_types.map((t) => (
                <Table.Row key={t.type} className="border-t border-border"><Table.Cell className="py-1 font-mono">{t.type}</Table.Cell><Table.Cell>{t.count}</Table.Cell></Table.Row>
              ))}
              {data.edge_types.length === 0 && <Table.Row><Table.Cell colSpan={2} className="py-2 text-muted-foreground">No edges yet.</Table.Cell></Table.Row>}
            </Table.Body>
          </Table>
        </Panel>
      )}
    </div>
  );
}
