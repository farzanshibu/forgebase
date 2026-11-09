import { useState } from "react";
import { billingStats } from "../../api";
import { Button, Loading, Panel, inputCls } from "../../components/ui";
import { useLoad } from "../../hooks";
import { formatBp } from "./money";
import { BillingError, Empty, Money, ROW, TABLE, THEAD, Th, when_ } from "./shared";

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
              <div key={r.currency} className="rounded-lg border border-zinc-800 bg-zinc-900/60 p-4">
                <p className="text-xs uppercase tracking-wide text-zinc-500">{r.currency} recurring</p>
                <p className="mt-1 text-sm text-zinc-400">MRR <span className="text-xl font-semibold text-zinc-100"><Money amount={r.mrr} currency={r.currency} /></span></p>
                <p className="text-sm text-zinc-400">ARR <span className="text-zinc-100"><Money amount={r.arr} currency={r.currency} /></span></p>
                <p className="text-sm text-zinc-400">{r.subscriptions} subscriptions</p>
              </div>
            ))}
          </section>
          <Panel title="Subscriptions in the window">
            <dl aria-label="Subscription metrics" className="grid gap-3 text-sm sm:grid-cols-5">
              <div><dt className="text-zinc-500">Active at start</dt><dd>{d.active_at_start}</dd></div>
              <div><dt className="text-zinc-500">New</dt><dd>{d.new_subscriptions}</dd></div>
              <div><dt className="text-zinc-500">Churned</dt><dd>{d.churned}</dd></div>
              <div><dt className="text-zinc-500">Churn rate</dt><dd>{formatBp(d.churn_rate_bp)}</dd></div>
              <div><dt className="text-zinc-500">Failed payments</dt><dd>{d.failed_payments}</dd></div>
            </dl>
            <p className="mt-3 text-sm text-zinc-400">
              By status: {Object.entries(d.by_status).map(([k, v]) => `${v} ${k}`).join(" · ") || "none"}
            </p>
          </Panel>
          <Panel title="Revenue by currency">
            {d.revenue.length === 0 ? <Empty>No payments in this window.</Empty> : (
              <table aria-label="Revenue" className={TABLE}>
                <thead className={THEAD}><tr><Th>Period</Th><Th>Currency</Th><Th>Gross</Th><Th>Refunded</Th><Th>Net</Th><Th>Payments</Th></tr></thead>
                <tbody>
                  {d.revenue.map((p) => (
                    <tr key={`${p.start}:${p.currency}`} className={ROW}>
                      <td className="py-1.5 pr-3 text-zinc-400">{when_(p.start)}</td><td>{p.currency}</td>
                      <td><Money amount={p.gross} currency={p.currency} /></td><td><Money amount={p.refunded} currency={p.currency} /></td>
                      <td><Money amount={p.net} currency={p.currency} /></td><td>{p.payments}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            )}
          </Panel>
        </>
      )}
    </div>
  );
}
