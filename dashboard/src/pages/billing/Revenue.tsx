import { useState } from "react";
import { billingStats } from "../../api";
import { Button, Loading, Panel, inputCls } from "../../components/ui";
import { useLoad } from "../../hooks";
import { formatBp } from "./money";
import { BillingError, Empty, Money, Th } from "./shared";
import { ROW, TABLE, THEAD, when_ } from "./util";
import { Table } from "panelui-web/table";

export default function Revenue() {
  const [interval, setInterval_] = useState<"day" | "month">("day");
  const stats = useLoad((s) => billingStats({ interval }, s), `stats:${interval}`);
  const d = stats.data;
  return (
    <div className="space-y-4">
      <div className="flex items-end gap-3 text-sm">
        <label>Interval
          <select className={`${inputCls} ml-2`} value={interval} onChange={(e) => setInterval_(e.target.value as "day" | "month")}>
            <option value="day">day (last 30 days)</option>
            <option value="month">month</option>
          </select>
        </label>
        <Button onClick={stats.reload}>Refresh</Button>
      </div>
      <BillingError error={stats.error} />
      {stats.loading && !stats.error && <Loading what="Loading revenue" />}
      {d && (
        <>
          <section aria-label="Recurring revenue" className="grid gap-3 sm:grid-cols-2 lg:grid-cols-3">
            {d.recurring.length === 0 && <Empty>No recurring revenue yet.</Empty>}
            {d.recurring.map((r) => (
              <div key={r.currency} className="rounded-lg border border-border bg-card p-4">
                <p className="text-xs uppercase tracking-wide text-muted-foreground">{r.currency} recurring</p>
                <p className="mt-1 text-sm text-muted-foreground">MRR <span className="text-xl font-semibold text-foreground"><Money amount={r.mrr} currency={r.currency} /></span></p>
                <p className="text-sm text-muted-foreground">ARR <span className="text-foreground"><Money amount={r.arr} currency={r.currency} /></span></p>
                <p className="text-sm text-muted-foreground">{r.subscriptions} subscriptions</p>
              </div>
            ))}
          </section>
          <Panel title="Subscriptions in the window">
            <dl aria-label="Subscription metrics" className="grid gap-3 text-sm sm:grid-cols-5">
              <div><dt className="text-muted-foreground">Active at start</dt><dd>{d.active_at_start}</dd></div>
              <div><dt className="text-muted-foreground">New</dt><dd>{d.new_subscriptions}</dd></div>
              <div><dt className="text-muted-foreground">Churned</dt><dd>{d.churned}</dd></div>
              <div><dt className="text-muted-foreground">Churn rate</dt><dd>{formatBp(d.churn_rate_bp)}</dd></div>
              <div><dt className="text-muted-foreground">Failed payments</dt><dd>{d.failed_payments}</dd></div>
            </dl>
            <p className="mt-3 text-sm text-muted-foreground">
              By status: {Object.entries(d.by_status).map(([k, v]) => `${v} ${k}`).join(" · ") || "none"}
            </p>
          </Panel>
          <Panel title="Revenue by currency">
            {d.revenue.length === 0 ? <Empty>No payments in this window.</Empty> : (
              <Table aria-label="Revenue" className={TABLE}>
                <Table.Header className={THEAD}><Table.Row><Th>Period</Th><Th>Currency</Th><Th>Gross</Th><Th>Refunded</Th><Th>Net</Th><Th>Payments</Th></Table.Row></Table.Header>
                <Table.Body>
                  {d.revenue.map((p) => (
                    <Table.Row key={`${p.start}:${p.currency}`} className={ROW}>
                      <Table.Cell className="py-1.5 pr-3 text-muted-foreground">{when_(p.start)}</Table.Cell><Table.Cell>{p.currency}</Table.Cell>
                      <Table.Cell><Money amount={p.gross} currency={p.currency} /></Table.Cell><Table.Cell><Money amount={p.refunded} currency={p.currency} /></Table.Cell>
                      <Table.Cell><Money amount={p.net} currency={p.currency} /></Table.Cell><Table.Cell>{p.payments}</Table.Cell>
                    </Table.Row>
                  ))}
                </Table.Body>
              </Table>
            )}
          </Panel>
        </>
      )}
    </div>
  );
}
