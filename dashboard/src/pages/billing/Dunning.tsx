import { Link } from "react-router-dom";
import { getBillingSettings, listDunning, listSubscriptions, payInvoice, runBillingMaintenance } from "../../api";
import { Button, ErrorView, Loading, Panel } from "../../components/ui";
import { useLoad } from "../../hooks";
import { Badge, BillingError, Empty, Money, Th, SubjectLink } from "./shared";
import { ROW, TABLE, THEAD, when_ } from "./util";
import { useAct } from "./useAct";
import { Table } from "panelui-web/table";

/** Dunning queue from the server (open invoices with a failed attempt or a scheduled retry) plus past-due subscriptions. */
export default function Dunning() {
  const invoices = useLoad((s) => listDunning(500, s), "dunning-queue");
  const subs = useLoad((s) => listSubscriptions(undefined, s), "dunning-subs");
  const settings = useLoad(getBillingSettings, "dunning-settings");
  const reload = () => { invoices.reload(); subs.reload(); };
  const act = useAct(reload);
  const queue = invoices.data ?? [];
  const pastDue = (subs.data ?? []).filter((s) => s.status === "past_due");
  return (
    <div className="space-y-4">
      <div className="flex flex-wrap items-center gap-3 text-sm text-muted-foreground">
        {settings.data && <span aria-label="Retry schedule">Retries after {settings.data.retry_days.map((d) => `${d}d`).join(", ")}; then {settings.data.final_action === "cancel" ? "cancel the subscription" : "leave it past due"}.</span>}
        <Button disabled={act.busy} onClick={() => act.run(runBillingMaintenance, "Maintenance ran")}>Run dunning now</Button>
        <Button onClick={reload}>Refresh</Button>
      </div>
      <BillingError error={invoices.error} />
      <ErrorView error={act.error} />
      {act.note && <p role="status" className="text-sm text-success">{act.note}</p>}
      {invoices.loading && !invoices.error && <Loading what="Loading dunning queue" />}
      {invoices.data && queue.length === 0 && <Empty>The dunning queue is empty: no invoice is waiting on a retry.</Empty>}
      {queue.length > 0 && (
        <Table aria-label="Dunning queue" className={TABLE}>
          <Table.Header className={THEAD}><Table.Row><Th>Invoice</Th><Th>Subject</Th><Th>Due</Th><Th>Attempts</Th><Th>Next attempt</Th><Th>Retries left</Th><Th>Last failure</Th><Th /></Table.Row></Table.Header>
          <Table.Body>
            {queue.map((i) => {
              return (
                <Table.Row key={i.invoice_id} className={ROW}>
                  <Table.Cell className="py-2 pr-3"><Link className="font-mono text-xs text-primary hover:underline" to={`/billing/invoices/${encodeURIComponent(i.invoice_id)}`}>{i.number ?? i.invoice_id}</Link></Table.Cell>
                  <Table.Cell><SubjectLink subject={i.subject} /></Table.Cell><Table.Cell><Money amount={i.amount_due} currency={i.currency} /></Table.Cell><Table.Cell>{i.attempt_count}</Table.Cell>
                  <Table.Cell className="text-muted-foreground">{i.next_attempt_at ? when_(i.next_attempt_at) : "none scheduled"}</Table.Cell>
                  <Table.Cell>{i.retries_left}</Table.Cell>
                  <Table.Cell className="text-destructive" title={i.last_failure_message ?? undefined}>{i.last_failure_code ?? ""}</Table.Cell>
                  <Table.Cell><Button disabled={act.busy} aria-label={`Retry ${i.number ?? i.invoice_id}`} onClick={() => act.run(() => payInvoice(i.invoice_id), "Retry attempted")}>Retry now</Button></Table.Cell>
                </Table.Row>
              );
            })}
          </Table.Body>
        </Table>
      )}
      <Panel title="Past-due subscriptions">
        {subs.data && pastDue.length === 0 && <Empty>No past-due subscriptions.</Empty>}
        {pastDue.length > 0 && (
          <ul aria-label="Past-due subscriptions" className="space-y-1 text-sm">
            {pastDue.map((s) => <li key={s.id}><SubjectLink subject={s.subject} /> <Badge status={s.status} /> <span className="text-muted-foreground">period ended {when_(s.current_period_end)}</span></li>)}
          </ul>
        )}
      </Panel>
    </div>
  );
}
