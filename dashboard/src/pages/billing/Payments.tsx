import { useState } from "react";
import { Link } from "react-router-dom";
import { listPayments, listRefunds } from "../../api";
import type { PaymentStatus } from "../../api";
import { Loading, Panel, inputCls } from "../../components/ui";
import { useLoad } from "../../hooks";
import { Badge, BillingError, Empty, Money, Th, SubjectLink } from "./shared";
import { ROW, TABLE, THEAD, when_ } from "./util";
import { Table } from "panelui-web/table";
import { Input } from "panelui-web/input";

export default function Payments() {
  const [status, setStatus] = useState<PaymentStatus | "">("");
  const [subject, setSubject] = useState("");
  const payments = useLoad((s) => listPayments(subject || undefined, 200, s), `payments:${subject}`);
  const refunds = useLoad((s) => listRefunds(subject || undefined, 200, s), `refunds:${subject}`);
  const rows = (payments.data ?? []).filter((p) => !status || p.status === status);
  const failed = (payments.data ?? []).filter((p) => p.status === "failed").length;
  return (
    <div className="space-y-4">
      <div className="flex flex-wrap items-end gap-3 text-sm">
        <label>Status<select className={`${inputCls} ml-2`} value={status} onChange={(e) => setStatus(e.target.value as PaymentStatus | "")}><option value="">all</option>{["pending", "succeeded", "failed", "canceled"].map((s) => <option key={s}>{s}</option>)}</select></label>
        <label>Subject<Input size="sm" className="ml-2 w-56" value={subject} onChange={(e) => setSubject(e.target.value)} placeholder="all subjects" /></label>
        {payments.data && <span aria-label="Failed payments" className={failed ? "text-destructive" : "text-muted-foreground"}>{failed} failed</span>}
      </div>
      <BillingError error={payments.error} />
      {payments.loading && !payments.error && <Loading what="Loading payments" />}
      {payments.data && rows.length === 0 && <Empty>No payments match.</Empty>}
      {rows.length > 0 && (
        <Table aria-label="Payments" className={TABLE}>
          <Table.Header className={THEAD}><Table.Row><Th>Payment</Th><Th>Subject</Th><Th>Status</Th><Th>Amount</Th><Th>Refunded</Th><Th>Provider</Th><Th>Invoice</Th><Th>Failure</Th><Th>When</Th></Table.Row></Table.Header>
          <Table.Body>
            {rows.map((p) => (
              <Table.Row key={p.id} className={ROW}>
                <Table.Cell className="py-2 pr-3 font-mono text-xs">{p.id}</Table.Cell><Table.Cell><SubjectLink subject={p.subject} /></Table.Cell><Table.Cell><Badge status={p.status} /></Table.Cell>
                <Table.Cell><Money amount={p.amount} currency={p.currency} /></Table.Cell><Table.Cell>{p.amount_refunded ? <Money amount={p.amount_refunded} currency={p.currency} /> : "-"}</Table.Cell>
                <Table.Cell>{p.provider}</Table.Cell><Table.Cell><Link className="font-mono text-xs text-primary hover:underline" to={`/billing/invoices/${encodeURIComponent(p.invoice_id)}`}>{p.invoice_id}</Link></Table.Cell>
                <Table.Cell className="text-destructive">{p.failure_code ? `${p.failure_code}${p.failure_message ? `: ${p.failure_message}` : ""}` : ""}</Table.Cell>
                <Table.Cell className="text-muted-foreground">{when_(p.created_at)}</Table.Cell>
              </Table.Row>
            ))}
          </Table.Body>
        </Table>
      )}
      <Panel title="Refunds">
        {refunds.data && refunds.data.length === 0 && <Empty>No refunds.</Empty>}
        {refunds.data && refunds.data.length > 0 && (
          <Table aria-label="Refunds" className={TABLE}>
            <Table.Header className={THEAD}><Table.Row><Th>Refund</Th><Th>Payment</Th><Th>Status</Th><Th>Amount</Th><Th>Reason</Th><Th>When</Th></Table.Row></Table.Header>
            <Table.Body>{refunds.data.map((r) => <Table.Row key={r.id} className={ROW}><Table.Cell className="py-1.5 pr-3 font-mono text-xs">{r.id}</Table.Cell><Table.Cell className="font-mono text-xs">{r.payment_id}</Table.Cell><Table.Cell><Badge status={r.status} /></Table.Cell><Table.Cell><Money amount={r.amount} currency={r.currency} /></Table.Cell><Table.Cell>{r.reason ?? "-"}</Table.Cell><Table.Cell className="text-muted-foreground">{when_(r.created_at)}</Table.Cell></Table.Row>)}</Table.Body>
          </Table>
        )}
      </Panel>
    </div>
  );
}
