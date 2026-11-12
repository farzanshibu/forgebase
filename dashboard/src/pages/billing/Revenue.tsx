import { useState } from "react";
import { EmptyState } from "../../components/EmptyState";
import { billingStats } from "../../api";
import { Button } from "../../components/ui";
import { Sel } from "../../components/controls";
import { SkeletonCards } from "../../components/Skeleton";
import { useLoad } from "../../hooks";
import { formatBp, formatCount } from "./money";
import { BillingError, Filter, Money, TableCard } from "./shared";
import { Table } from "panelui-web/table";
import { Badge } from "../../components/StatusBadge";
import { Stat, StatGrid } from "../../components/Stats";
import { Th } from "../../components/TableCard";
import { When } from "../../components/Time";

type Interval = "day" | "month";

export default function Revenue() {
  const [interval, setInterval_] = useState<Interval>("day");
  const stats = useLoad((s) => billingStats({ interval }, s), `stats:${interval}`);
  const d = stats.data;
  // One card per currency: recurring revenue and what was collected in the window.
  const currencies = d ? [...new Set([...d.recurring.map((r) => r.currency), ...d.revenue.map((r) => r.currency)])] : [];
  const net = (c: string) => (d?.revenue ?? []).filter((r) => r.currency === c).reduce((sum, r) => sum + r.net, 0);
  const statuses = d ? Object.entries(d.by_status).filter(([, n]) => n > 0) : [];
  return (
    <div className="space-y-5">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <Filter label="Interval">
          {(id) => <Sel id={id} value={interval} onChange={(v) => setInterval_(v as Interval)} options={[{ value: "day", label: "By day, last 30 days" }, { value: "month", label: "By month" }]} className="w-52" />}
        </Filter>
        <Button onClick={stats.reload} disabled={stats.loading}>Refresh</Button>
      </div>
      <BillingError error={stats.error} />
      {stats.loading && !stats.error && <SkeletonCards count={3} label="Loading revenue" />}
      {d && (
        <>
          <section aria-label="Recurring revenue" className="grid gap-3 md:grid-cols-2 xl:grid-cols-3">
            {currencies.length === 0 && (
              <EmptyState compact className="md:col-span-2 xl:col-span-3" title="No recurring revenue yet" why="Monthly and annual recurring revenue appear here once a customer has an active subscription." />
            )}
            {currencies.map((c) => {
              const r = d.recurring.find((x) => x.currency === c);
              return (
                <article key={c} aria-label={`${c} revenue`} className="rounded-xl border border-border bg-card p-4">
                  <div className="flex items-center justify-between gap-2">
                    <span className="rounded-md bg-muted px-1.5 py-0.5 font-mono text-xs font-medium">{c}</span>
                    <span className="text-xs text-muted-foreground tabular-nums">{r ? `${formatCount(r.subscriptions)} active subscriptions` : "no subscriptions"}</span>
                  </div>
                  <p className="mt-3 text-xs font-medium text-muted-foreground">Monthly recurring revenue</p>
                  <p className="text-3xl font-semibold tracking-tight"><Money amount={r?.mrr ?? 0} currency={c} /></p>
                  <dl className="mt-3 grid grid-cols-2 gap-2 border-t border-border pt-3 text-sm">
                    <div><dt className="text-xs text-muted-foreground">Annual run rate</dt><dd><Money amount={r?.arr ?? 0} currency={c} /></dd></div>
                    <div><dt className="text-xs text-muted-foreground">Net collected in window</dt><dd><Money amount={net(c)} currency={c} /></dd></div>
                  </dl>
                </article>
              );
            })}
          </section>
          <StatGrid label="Subscription metrics" columns={5}>
            <Stat label="Active at start" value={formatCount(d.active_at_start)} />
            <Stat label="New" value={formatCount(d.new_subscriptions)} tone={d.new_subscriptions > 0 ? "ok" : undefined} />
            <Stat label="Churned" value={formatCount(d.churned)} />
            <Stat label="Churn rate" value={formatBp(d.churn_rate_bp)} />
            <Stat label="Failed payments" value={formatCount(d.failed_payments)} tone={d.failed_payments > 0 ? "bad" : undefined} />
          </StatGrid>
          <div aria-label="Subscriptions by status" className="flex flex-wrap items-center gap-2 text-sm text-muted-foreground">
            <span className="text-xs font-medium">Subscriptions by status</span>
            {statuses.length === 0 && <span>none yet</span>}
            {statuses.map(([k, v]) => <Badge key={k} status={k}>{`${formatCount(v)} ${k.replace(/_/g, " ")}`}</Badge>)}
          </div>
          <TableCard title="Revenue by period" description="Payments collected, per period and currency. Refunds are taken off the net.">
            {d.revenue.length === 0 ? (
              <EmptyState compact title="No payments in this window" why="Pick a longer interval to see earlier payments." />
            ) : (
              <Table framed aria-label="Revenue">
                <Table.Header><Table.Row><Th>Period</Th><Th>Currency</Th><Th align="end">Gross</Th><Th align="end">Refunded</Th><Th align="end">Net</Th><Th align="end">Payments</Th></Table.Row></Table.Header>
                <Table.Body>
                  {d.revenue.map((p) => (
                    <Table.Row key={`${p.start}:${p.currency}`}>
                      <Table.Cell className="text-muted-foreground"><When iso={p.start} day /></Table.Cell>
                      <Table.Cell className="font-mono text-xs">{p.currency}</Table.Cell>
                      <Table.Cell align="end"><Money amount={p.gross} currency={p.currency} /></Table.Cell>
                      <Table.Cell align="end" className="text-muted-foreground">{p.refunded ? <Money amount={-p.refunded} currency={p.currency} /> : <Money amount={0} currency={p.currency} />}</Table.Cell>
                      <Table.Cell align="end" className="font-medium"><Money amount={p.net} currency={p.currency} /></Table.Cell>
                      <Table.Cell align="end">{formatCount(p.payments)}</Table.Cell>
                    </Table.Row>
                  ))}
                </Table.Body>
              </Table>
            )}
          </TableCard>
        </>
      )}
    </div>
  );
}
