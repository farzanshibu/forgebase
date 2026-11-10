import { useState } from "react";
import { clearSlowQueries, setSlowThreshold, slowQueries } from "../../api";
import { useLoad } from "../../hooks";
import { Button, ConfirmButton, ErrorView, Loading } from "../../components/ui";
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
      {loading && !data && !error && <Loading what="Loading slow queries" />}
      {data && (
        <>
          <div className="flex flex-wrap items-end gap-3 text-sm">
            <span className="text-muted-foreground">Statements slower than <strong className="text-foreground">{data.threshold_ms} ms</strong> are logged.</span>
            <Input size="sm" aria-label="Threshold ms" className="w-24" inputMode="numeric" value={threshold} onChange={(e) => setThreshold(e.target.value)} placeholder="ms" />
            <Button disabled={!/^\d+$/.test(threshold)} onClick={() => void act(() => setSlowThreshold(Number(threshold)))}>Set threshold</Button>
            <Button onClick={reload}>Refresh</Button>
            <ConfirmButton label="Clear log" disabled={data.items.length === 0} onConfirm={() => void act(clearSlowQueries)} />
          </div>
          {data.items.length === 0 ? (
            <p className="text-sm text-muted-foreground">No slow queries recorded.</p>
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
