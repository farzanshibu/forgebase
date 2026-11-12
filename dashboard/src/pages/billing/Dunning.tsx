import { Link } from "react-router-dom";
import { EmptyState } from "../../components/EmptyState";
import { getBillingSettings, listDunning, listSubscriptions, payInvoice, runBillingMaintenance } from "../../api";
import { HelpTip } from "../../components/HelpTip";
import { Button, ErrorView } from "../../components/ui";
import { SkeletonTable } from "../../components/Skeleton";
import { useLoad } from "../../hooks";
import { formatCount } from "./money";
import { BillingError, Money, None, Note, SubjectLink, TableCard } from "./shared";
import { useAct } from "./useAct";
import { Table } from "panelui-web/table";
import { Badge } from "../../components/StatusBadge";
import { Th } from "../../components/TableCard";
import { When } from "../../components/Time";

/** Dunning queue from the server (open invoices with a failed attempt or a scheduled retry) plus past-due subscriptions. */
export default function Dunning() {
  const invoices = useLoad((s) => listDunning(500, s), "dunning-queue");
  const subs = useLoad((s) => listSubscriptions(undefined, s), "dunning-subs");
  const settings = useLoad(getBillingSettings, "dunning-settings");
  const reload = () => { invoices.reload(); subs.reload(); };
  const act = useAct(reload);
  const queue = invoices.data ?? [];
  const pastDue = (subs.data ?? []).filter((s) => s.status === "past_due");
  const run = <Button disabled={act.busy} onClick={() => act.run(runBillingMaintenance, "Maintenance ran")}>{act.busy ? "Running…" : "Run dunning now"}</Button>;
  return (
    <div className="space-y-5">
      <div className="flex flex-wrap items-center justify-between gap-3 rounded-xl border border-border bg-muted/30 px-4 py-3 text-sm">
        <p className="flex flex-wrap items-center gap-1 text-muted-foreground">
          <span className="font-medium text-foreground">Dunning</span> <HelpTip term="dunning" />
          {settings.data && <span aria-label="Retry schedule">Retries after {settings.data.retry_days.map((d) => `${d}d`).join(", ")}; then {settings.data.final_action === "cancel" ? "cancel the subscription" : "leave it past due"}.</span>}
          <Link to="/billing/settings" className="text-primary underline-offset-2 hover:underline">Change</Link>
        </p>
        <div className="flex gap-2">
          <Button onClick={reload}>Refresh</Button>
          {run}
        </div>
      </div>
      <ErrorView error={act.error} />
      <Note>{act.note}</Note>
      <BillingError error={invoices.error} />
      <TableCard title="Retry queue" count={invoices.data ? queue.length : undefined} description="Open invoices whose payment failed, with the next automatic retry.">
        {invoices.loading && !invoices.error && <SkeletonTable rows={3} columns={6} label="Loading dunning queue" />}
        {invoices.data && queue.length === 0 && <EmptyState compact title="The retry queue is empty" why="No invoice is waiting on a payment retry." />}
        {queue.length > 0 && (
          <Table framed aria-label="Dunning queue">
            <Table.Header><Table.Row><Th>Invoice</Th><Th>Customer</Th><Th align="end">Amount due</Th><Th align="end">Attempts</Th><Th>Next attempt</Th><Th align="end">Retries left</Th><Th>Last failure</Th><Th><span className="sr-only">Actions</span></Th></Table.Row></Table.Header>
            <Table.Body>
              {queue.map((i) => (
                <Table.Row key={i.invoice_id}>
                  <Table.Cell><Link className="font-mono text-xs font-medium text-primary hover:underline" to={`/billing/invoices/${encodeURIComponent(i.invoice_id)}`}>{i.number ?? i.invoice_id}</Link></Table.Cell>
                  <Table.Cell><SubjectLink subject={i.subject} /></Table.Cell>
                  <Table.Cell align="end" className="font-medium"><Money amount={i.amount_due} currency={i.currency} /></Table.Cell>
                  <Table.Cell align="end">{formatCount(i.attempt_count)}</Table.Cell>
                  <Table.Cell className="text-muted-foreground">{i.next_attempt_at ? <When iso={i.next_attempt_at} /> : "none scheduled"}</Table.Cell>
                  <Table.Cell align="end">{formatCount(i.retries_left)}</Table.Cell>
                  <Table.Cell className="font-mono text-xs text-destructive" title={i.last_failure_message ?? undefined}>{i.last_failure_code ?? <None />}</Table.Cell>
                  <Table.Cell className="text-right"><Button disabled={act.busy} aria-label={`Retry ${i.number ?? i.invoice_id}`} onClick={() => act.run(() => payInvoice(i.invoice_id), "Retry attempted")}>Retry now</Button></Table.Cell>
                </Table.Row>
              ))}
            </Table.Body>
          </Table>
        )}
      </TableCard>
      <TableCard title="Past-due subscriptions" count={subs.data ? pastDue.length : undefined} description="Subscriptions whose last payment failed. They return to active once an invoice is paid.">
        {subs.data && pastDue.length === 0 && <EmptyState compact title="No past-due subscriptions" why="A subscription shows here when its payment failed." />}
        {pastDue.length > 0 && (
          <ul aria-label="Past-due subscriptions" className="divide-y divide-border text-sm">
            {pastDue.map((s) => (
              <li key={s.id} className="flex flex-wrap items-center justify-between gap-2 px-4 py-2.5">
                <span className="flex items-center gap-2"><SubjectLink subject={s.subject} /><Badge status={s.status} /></span>
                <span className="text-xs text-muted-foreground">period ended <When iso={s.current_period_end} day /></span>
              </li>
            ))}
          </ul>
        )}
      </TableCard>
    </div>
  );
}
