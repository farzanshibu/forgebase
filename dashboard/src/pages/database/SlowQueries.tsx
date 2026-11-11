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

export default function SlowQueries() {
  const { data, error, loading, reload } = useLoad(slowQueries, "slow");
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
    <div className="space-y-4">
      <ErrorView error={error} />
      <ErrorView error={problem} />
      {loading && !data && !error && <SkeletonTable rows={3} columns={4} label="Loading slow queries" />}
      {data && (
        <>
          <div className="flex flex-wrap items-end gap-3 text-sm">
            <span className="text-muted-foreground">Statements slower than <strong className="text-foreground">{data.threshold_ms} ms</strong> are logged.</span>
            <Input size="sm" aria-label="Threshold ms" className="w-24" inputMode="numeric" value={threshold} onChange={(e) => setThreshold(e.target.value)} placeholder="ms" />
            <Button disabled={!/^\d+$/.test(threshold)} onClick={() => void act(async () => { await setSlowThreshold(Number(threshold)); notify.saved("Threshold"); })}>Set threshold</Button>
            <Button onClick={reload}>Refresh</Button>
            <ConfirmDialog
              trigger={<Button tone="danger" disabled={data.items.length === 0}>Clear log</Button>}
              title="Clear the slow-query log?"
              consequence="The recorded statements and the index recommendations built from them are removed."
              confirmLabel="Clear log"
              onConfirm={async () => { await clearSlowQueries(); notify.deleted("Slow-query log"); reload(); }}
            />
          </div>
          {data.items.length === 0 ? (
            <EmptyState compact title="No slow queries recorded" why={`Statements that take longer than ${data.threshold_ms} ms are logged here, so you can see what to index.`} />
          ) : (
            <Table className="w-full text-left text-sm">
              <Table.Header className="text-xs uppercase tracking-wide text-muted-foreground"><Table.Row><Table.Head className="py-1">SQL</Table.Head><Table.Head>Duration</Table.Head><Table.Head>Rows</Table.Head><Table.Head>When</Table.Head></Table.Row></Table.Header>
              <Table.Body>
                {data.items.map((q, i) => (
                  <Table.Row key={i} className="border-t border-border align-top">
                    <Table.Cell className="max-w-xl py-1 font-mono text-xs break-all">{q.sql}{q.label && <span className="ml-2 text-muted-foreground">[{q.label.collection}]</span>}</Table.Cell>
                    <Table.Cell>{q.duration_ms.toFixed(1)} ms</Table.Cell>
                    <Table.Cell>{q.rows}</Table.Cell>
                    <Table.Cell className="text-xs text-muted-foreground">{q.at}</Table.Cell>
                  </Table.Row>
                ))}
              </Table.Body>
            </Table>
          )}
        </>
      )}
    </div>
  );
}
