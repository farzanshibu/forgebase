import { useState } from "react";
import { clearSlowQueries, setSlowThreshold, slowQueries } from "../../api";
import { useLoad } from "../../hooks";
import { ConfirmDialog } from "../../components/ConfirmDialog";
import { EmptyState } from "../../components/EmptyState";
import { SkeletonTable } from "../../components/Skeleton";
import { Button, ErrorView } from "../../components/ui";
import { notify } from "../../toaster";
import { Table } from "panelui-web/table";
import { Input } from "panelui-web/input";
import { getDatabaseInfo } from "./info";
import TopQueries from "./TopQueries";
import { TableCard } from "./parts";
import { when } from "../functions/util";

export default function SlowQueries() {
  const { data, error, loading, reload } = useLoad(slowQueries, "slow");
  const info = useLoad(getDatabaseInfo, "database-info");
  const [threshold, setThreshold] = useState("");
  const [problem, setProblem] = useState<unknown>();

  async function act(fn: () => Promise<unknown>) {
    setProblem(undefined);
    try {
      await fn();
      reload();
    } catch (e) {
      setProblem(e);
    }
  }

  return (
    <div className="space-y-6">
      <ErrorView error={error} />
      <ErrorView error={problem} />
      {loading && !data && !error && <SkeletonTable rows={3} columns={4} label="Loading slow queries" />}
      {data && (
        <TableCard
          title="Slow-query log"
          count={data.items.length}
          description={<>Statements slower than <strong className="text-foreground">{data.threshold_ms} ms</strong> are logged.</>}
          actions={
            <>
              <form className="flex items-center gap-2" onSubmit={(e) => { e.preventDefault(); if (/^\d+$/.test(threshold)) void act(async () => { await setSlowThreshold(Number(threshold)); notify.saved("Threshold"); }); }}>
                <span className="relative">
                  <Input size="sm" aria-label="Threshold ms" className="w-28 pr-8 tabular-nums" inputMode="numeric" value={threshold} onChange={(e) => setThreshold(e.target.value)} placeholder={String(data.threshold_ms)} />
                  <span className="pointer-events-none absolute inset-y-0 right-2.5 flex items-center text-xs text-muted-foreground">ms</span>
                </span>
                <Button type="submit" disabled={!/^\d+$/.test(threshold)}>Set threshold</Button>
              </form>
              <Button onClick={reload}>Refresh</Button>
              <ConfirmDialog
                trigger={<Button tone="danger" disabled={data.items.length === 0}>Clear log</Button>}
                title="Clear the slow-query log?"
                consequence="The recorded statements and the index recommendations built from them are removed."
                confirmLabel="Clear log"
                onConfirm={async () => { await clearSlowQueries(); notify.deleted("Slow-query log"); reload(); }}
              />
            </>
          }
        >
          {data.items.length === 0 ? (
            <div className="p-4"><EmptyState compact title="No slow queries recorded" why={`Statements that take longer than ${data.threshold_ms} ms are logged here, so you can see what to index.`} /></div>
          ) : (
            <Table framed density="compact" aria-label="Slow queries">
              <Table.Header className="bg-muted/40"><Table.Row><Table.Head>Statement</Table.Head><Table.Head align="end">Duration</Table.Head><Table.Head align="end">Rows</Table.Head><Table.Head align="end">When</Table.Head></Table.Row></Table.Header>
              <Table.Body>
                {data.items.map((q, i) => (
                  <Table.Row key={i} className="align-top">
                    <Table.Cell className="max-w-xl py-2 font-mono text-xs break-all">{q.sql}{q.label && <span className="ml-2 rounded bg-muted px-1.5 py-0.5 font-sans text-muted-foreground">{q.label.collection}</span>}</Table.Cell>
                    <Table.Cell align="end" className="py-2 whitespace-nowrap">{q.duration_ms.toFixed(1)} ms</Table.Cell>
                    <Table.Cell align="end" className="py-2">{q.rows}</Table.Cell>
                    <Table.Cell align="end" className="py-2 whitespace-nowrap text-xs text-muted-foreground">{when(q.at)}</Table.Cell>
                  </Table.Row>
                ))}
              </Table.Body>
            </Table>
          )}
        </TableCard>
      )}
      {info.data?.backend === "postgres" && <TopQueries />}
    </div>
  );
}
