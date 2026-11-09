import { Link } from "react-router-dom";
import { getBillingSettings, listInvoices, listPayments, listSubscriptions, payInvoice, runBillingMaintenance } from "../../api";
import { Button, ErrorView, Loading, Panel } from "../../components/ui";
import { useLoad } from "../../hooks";
import { Badge, BillingError, Empty, Money, ROW, TABLE, THEAD, Th, subjectLink, useAct, when_ } from "./shared";

/** Dunning queue: open invoices that failed collection plus past-due subscriptions, with the retry schedule from the settings. */
export default function Dunning() {
  const invoices = useLoad((s) => listInvoices({ status: "open", limit: 500 }, s), "dunning-invoices");
  const subs = useLoad((s) => listSubscriptions(undefined, s), "dunning-subs");
  const payments = useLoad((s) => listPayments(undefined, 500, s), "dunning-payments");
  const settings = useLoad(getBillingSettings, "dunning-settings");
  const reload = () => { invoices.reload(); subs.reload(); payments.reload(); };
  const act = useAct(reload);
  const queue = (invoices.data ?? []).filter((i) => i.attempt_count > 0 || i.next_attempt_at);
  const pastDue = (subs.data ?? []).filter((s) => s.status === "past_due");
  const lastFailure = (invoiceId: string) => (payments.data ?? []).find((p) => p.invoice_id === invoiceId && p.status === "failed");
  return (
    <div className="space-y-4">
      <div className="flex flex-wrap items-center gap-3 text-sm text-zinc-400">
        {settings.data && <span aria-label="Retry schedule">Retries after {settings.data.retry_days.map((d) => `${d}d`).join(", ")}; then {settings.data.final_action === "cancel" ? "cancel the subscription" : "leave it past due"}.</span>}
        <Button disabled={act.busy} onClick={() => act.run(runBillingMaintenance, "Maintenance ran")}>Run dunning now</Button>
        <Button onClick={reload}>Refresh</Button>
      </div>
      <BillingError error={invoices.error} />
      <ErrorView error={act.error} />
      {act.note && <p role="status" className="text-sm text-emerald-300">{act.note}</p>}
      {invoices.loading && !invoices.error && <Loading what="Loading dunning queue" />}
      {invoices.data && queue.length === 0 && <Empty>The dunning queue is empty: no invoice is waiting on a retry.</Empty>}
      {queue.length > 0 && (
        <table aria-label="Dunning queue" className={TABLE}>
          <thead className={THEAD}><tr><Th>Invoice</Th><Th>Subject</Th><Th>Due</Th><Th>Attempts</Th><Th>Next attempt</Th><Th>Last failure</Th><Th /></tr></thead>
          <tbody>
            {queue.map((i) => {
              const f = lastFailure(i.id);
              return (
                <tr key={i.id} className={ROW}>
                  <td className="py-2 pr-3"><Link className="font-mono text-xs text-amber-300 hover:underline" to={`/billing/invoices/${encodeURIComponent(i.id)}`}>{i.number ?? i.id}</Link></td>
                  <td>{subjectLink(i.subject)}</td><td><Money amount={i.amount_due} currency={i.currency} /></td><td>{i.attempt_count}</td>
                  <td className="text-zinc-400">{i.next_attempt_at ? when_(i.next_attempt_at) : "none scheduled"}</td>
                  <td className="text-red-300">{f?.failure_code ?? ""}</td>
                  <td><Button disabled={act.busy} aria-label={`Retry ${i.number ?? i.id}`} onClick={() => act.run(() => payInvoice(i.id), "Retry attempted")}>Retry now</Button></td>
                </tr>
              );
            })}
          </tbody>
        </table>
      )}
      <Panel title="Past-due subscriptions">
        {subs.data && pastDue.length === 0 && <Empty>No past-due subscriptions.</Empty>}
        {pastDue.length > 0 && (
          <ul aria-label="Past-due subscriptions" className="space-y-1 text-sm">
            {pastDue.map((s) => <li key={s.id}>{subjectLink(s.subject)} <Badge status={s.status} /> <span className="text-zinc-500">period ended {when_(s.current_period_end)}</span></li>)}
          </ul>
        )}
      </Panel>
    </div>
  );
}
