/**
 * Top queries (FB-155): the statements that used the most time, from `pg_stat_statements`, through the SQL editor
 * endpoint. Needs the extension: when it is not installed the panel offers to install it.
 */
import { useState } from "react";
import { Table } from "panelui-web/table";
import { createExtension, listExtensions } from "../../api";
import { EmptyState } from "../../components/EmptyState";
import { SkeletonTable } from "../../components/Skeleton";
import { Button, ErrorView, Panel } from "../../components/ui";
import { useLoad } from "../../hooks";
import { notify } from "../../toaster";
import { runSqlStatement } from "./sqlRequest";

export const TOP_QUERIES_SQL =
  "SELECT query, calls, total_exec_time, mean_exec_time, rows FROM pg_stat_statements " +
  "WHERE dbid = (SELECT oid FROM pg_database WHERE datname = current_database()) ORDER BY total_exec_time DESC LIMIT 20";

interface TopQuery { query: string; calls: number; total_ms: number; mean_ms: number; rows: number }

async function load(): Promise<{ installed: boolean; items: TopQuery[] }> {
  const ext = (await listExtensions()).find((e) => e.name === "pg_stat_statements");
  if (!ext?.installed_version) return { installed: false, items: [] };
  const r = await runSqlStatement(TOP_QUERIES_SQL);
  const n = (row: unknown[], c: string) => Number(row[r.columns.indexOf(c)] ?? 0);
  return {
    installed: true,
    items: r.rows.map((row) => ({ query: String(row[r.columns.indexOf("query")] ?? ""), calls: n(row, "calls"), total_ms: n(row, "total_exec_time"), mean_ms: n(row, "mean_exec_time"), rows: n(row, "rows") })),
  };
}

export default function TopQueries() {
  const { data, error, loading, reload } = useLoad(load, "top-queries");
  const [problem, setProblem] = useState<unknown>();
  async function install() {
    setProblem(undefined);
    try {
      await createExtension("pg_stat_statements");
      notify.saved("pg_stat_statements");
      reload();
    } catch (e) {
      setProblem(e);
    }
  }
  return (
    <Panel title="Top queries by total time" actions={<Button onClick={reload}>Refresh</Button>}>
      <ErrorView error={error} onRetry={reload} />
      <ErrorView error={problem} />
      {loading && !data && !error && <SkeletonTable rows={3} columns={5} label="Loading top queries" />}
      {data && !data.installed && (
        <EmptyState compact title="pg_stat_statements is not installed" why="This extension counts every statement's calls and time, so you can see which queries cost the most." action={<Button tone="primary" onClick={() => void install()}>Install pg_stat_statements</Button>} />
      )}
      {data?.installed && data.items.length === 0 && <EmptyState compact title="No statements counted yet" why="Statements appear here after the database has run them." />}
      {data?.installed && data.items.length > 0 && (
        <div className="overflow-x-auto">
          <Table aria-label="Top queries" className="w-full text-left text-sm">
            <Table.Header className="text-xs uppercase tracking-wide text-muted-foreground">
              <Table.Row><Table.Head className="py-1">SQL</Table.Head><Table.Head>Calls</Table.Head><Table.Head>Total</Table.Head><Table.Head>Mean</Table.Head><Table.Head>Rows</Table.Head></Table.Row>
            </Table.Header>
            <Table.Body>
              {data.items.map((q, i) => (
                <Table.Row key={i} className="border-t border-border align-top">
                  <Table.Cell className="max-w-xl py-1 font-mono text-xs break-all">{q.query}</Table.Cell>
                  <Table.Cell>{q.calls}</Table.Cell>
                  <Table.Cell className="whitespace-nowrap">{q.total_ms.toFixed(1)} ms</Table.Cell>
                  <Table.Cell className="whitespace-nowrap">{q.mean_ms.toFixed(2)} ms</Table.Cell>
                  <Table.Cell>{q.rows}</Table.Cell>
                </Table.Row>
              ))}
            </Table.Body>
          </Table>
        </div>
      )}
    </Panel>
  );
}
