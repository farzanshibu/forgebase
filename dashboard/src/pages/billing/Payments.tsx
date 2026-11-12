import { useState } from "react";
import { EmptyState } from "../../components/EmptyState";
import { Link } from "react-router-dom";
import { listPayments, listRefunds } from "../../api";
import type { PaymentStatus } from "../../api";
import { Sel } from "../../components/controls";
import { SkeletonTable } from "../../components/Skeleton";
import { useLoad } from "../../hooks";
import { formatCount } from "./money";
import { BillingError, Filter, Money, None, SubjectLink, TableCard } from "./shared";
import { Table } from "panelui-web/table";
import { Input } from "panelui-web/input";
import { Badge } from "../../components/StatusBadge";
import { IdText } from "../../components/IdText";
import { Stat, StatGrid } from "../../components/Stats";
import { Th } from "../../components/TableCard";
import { When } from "../../components/Time";

const STATUSES = ["pending", "succeeded", "failed", "canceled"] as const satisfies readonly PaymentStatus[];

export default function Payments() {
  const [status, setStatus] = useState<PaymentStatus | "">("");
  const [subject, setSubject] = useState("");
  const payments = useLoad((s) => listPayments(subject || undefined, 200, s), `payments:${subject}`);
  const refunds = useLoad((s) => listRefunds(subject || undefined, 200, s), `refunds:${subject}`);
  const all = payments.data ?? [];
  const rows = all.filter((p) => !status || p.status === status);
  const count = (s: PaymentStatus) => all.filter((p) => p.status === s).length;
  const toolbar = (
    <>
      <Filter label="Status">
        {(id) => <Sel placeholder="All statuses" id={id} value={status} onChange={setStatus} options={[{ value: "", label: "All statuses" }, ...STATUSES]} className="w-40" />}
      </Filter>
      <Filter label="Subject">
        {(id) => <Input id={id} size="sm" className="w-56 font-mono" value={subject} onChange={(e) => setSubject(e.target.value)} placeholder="All subjects" />}
      </Filter>
    </>
  );
  return (
    <div className="space-y-5">
      {payments.data && (
        <StatGrid label="Payment summary" columns={4}>
          <Stat label="Payments" value={formatCount(all.length)} hint={subject ? `for ${subject}` : "latest 200"} />
          <Stat label="Succeeded" value={formatCount(count("succeeded"))} tone={count("succeeded") ? "ok" : undefined} />
          <Stat label="Failed" value={formatCount(count("failed"))} tone={count("failed") ? "bad" : undefined} hint={count("failed") ? "see Dunning for retries" : undefined} />
          <Stat label="Refunds" value={formatCount(refunds.data?.length ?? 0)} />
        </StatGrid>
      )}
      <BillingError error={payments.error} />
      <TableCard title="Payments" count={payments.data ? rows.length : undefined} description="Every attempt to collect an invoice, with the provider's answer." toolbar={toolbar}>
        {payments.loading && !payments.error && <SkeletonTable rows={4} columns={7} label="Loading payments" />}
        {payments.data && rows.length === 0 && (
          all.length === 0 && !subject
            ? <EmptyState title="No payments yet" why="A payment is recorded each time an invoice is charged through a provider or marked paid." />
            : <EmptyState compact title="No payments match" why="Change the status or subject to see more." />
        )}
        {rows.length > 0 && (
          <Table framed aria-label="Payments">
            <Table.Header><Table.Row><Th>Payment</Th><Th>Customer</Th><Th>Status</Th><Th align="end">Amount</Th><Th align="end">Refunded</Th><Th>Provider</Th><Th>Invoice</Th><Th>Failure</Th><Th>When</Th></Table.Row></Table.Header>
            <Table.Body>
              {rows.map((p) => (
                <Table.Row key={p.id}>
                  <Table.Cell><IdText value={p.id} what="payment id" /></Table.Cell>
                  <Table.Cell><SubjectLink subject={p.subject} /></Table.Cell>
                  <Table.Cell><Badge status={p.status} /></Table.Cell>
                  <Table.Cell align="end" className="font-medium"><Money amount={p.amount} currency={p.currency} /></Table.Cell>
                  <Table.Cell align="end">{p.amount_refunded ? <Money amount={p.amount_refunded} currency={p.currency} /> : <None />}</Table.Cell>
                  <Table.Cell className="capitalize">{p.provider}</Table.Cell>
                  <Table.Cell><Link className="font-mono text-xs text-primary hover:underline" to={`/billing/invoices/${encodeURIComponent(p.invoice_id)}`}>{p.invoice_id}</Link></Table.Cell>
                  <Table.Cell className="max-w-64 text-xs text-destructive">
                    {p.failure_code ? <span className="line-clamp-2" title={p.failure_message ?? undefined}>{`${p.failure_code}${p.failure_message ? `: ${p.failure_message}` : ""}`}</span> : null}
                  </Table.Cell>
                  <Table.Cell className="text-muted-foreground"><When iso={p.created_at} /></Table.Cell>
                </Table.Row>
              ))}
            </Table.Body>
          </Table>
        )}
      </TableCard>
      <TableCard title="Refunds" count={refunds.data?.length} description="Money sent back to customers through credit notes.">
        {refunds.data && refunds.data.length === 0 && <EmptyState compact title="No refunds" why="Refunds appear here when a credit note sends money back to the customer." />}
        {refunds.data && refunds.data.length > 0 && (
          <Table framed aria-label="Refunds">
            <Table.Header><Table.Row><Th>Refund</Th><Th>Payment</Th><Th>Status</Th><Th align="end">Amount</Th><Th>Reason</Th><Th>When</Th></Table.Row></Table.Header>
            <Table.Body>
              {refunds.data.map((r) => (
                <Table.Row key={r.id}>
                  <Table.Cell><IdText value={r.id} what="refund id" /></Table.Cell>
                  <Table.Cell><IdText value={r.payment_id} what="payment id" /></Table.Cell>
                  <Table.Cell><Badge status={r.status} /></Table.Cell>
                  <Table.Cell align="end" className="font-medium"><Money amount={r.amount} currency={r.currency} /></Table.Cell>
                  <Table.Cell>{r.reason ?? <None />}</Table.Cell>
                  <Table.Cell className="text-muted-foreground"><When iso={r.created_at} /></Table.Cell>
                </Table.Row>
              ))}
            </Table.Body>
          </Table>
        )}
      </TableCard>
    </div>
  );
}
