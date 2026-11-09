import { useState } from "react";
import { Link } from "react-router-dom";
import { listPayments, listRefunds } from "../../api";
import type { PaymentStatus } from "../../api";
import { Loading, Panel, inputCls } from "../../components/ui";
import { useLoad } from "../../hooks";
import { Badge, BillingError, Empty, Money, Th, SubjectLink } from "./shared";
import { ROW, TABLE, THEAD, when_ } from "./util";

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
        <label>Subject<input className={`${inputCls} ml-2 w-56`} value={subject} onChange={(e) => setSubject(e.target.value)} placeholder="all subjects" /></label>
        {payments.data && <span aria-label="Failed payments" className={failed ? "text-red-300" : "text-zinc-400"}>{failed} failed</span>}
      </div>
      <BillingError error={payments.error} />
      {payments.loading && !payments.error && <Loading what="Loading payments" />}
      {payments.data && rows.length === 0 && <Empty>No payments match.</Empty>}
      {rows.length > 0 && (
        <table aria-label="Payments" className={TABLE}>
          <thead className={THEAD}><tr><Th>Payment</Th><Th>Subject</Th><Th>Status</Th><Th>Amount</Th><Th>Refunded</Th><Th>Provider</Th><Th>Invoice</Th><Th>Failure</Th><Th>When</Th></tr></thead>
          <tbody>
            {rows.map((p) => (
              <tr key={p.id} className={ROW}>
                <td className="py-2 pr-3 font-mono text-xs">{p.id}</td><td><SubjectLink subject={p.subject} /></td><td><Badge status={p.status} /></td>
                <td><Money amount={p.amount} currency={p.currency} /></td><td>{p.amount_refunded ? <Money amount={p.amount_refunded} currency={p.currency} /> : "-"}</td>
                <td>{p.provider}</td><td><Link className="font-mono text-xs text-amber-300 hover:underline" to={`/billing/invoices/${encodeURIComponent(p.invoice_id)}`}>{p.invoice_id}</Link></td>
                <td className="text-red-300">{p.failure_code ? `${p.failure_code}${p.failure_message ? `: ${p.failure_message}` : ""}` : ""}</td>
                <td className="text-zinc-400">{when_(p.created_at)}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
      <Panel title="Refunds">
        {refunds.data && refunds.data.length === 0 && <Empty>No refunds.</Empty>}
        {refunds.data && refunds.data.length > 0 && (
          <table aria-label="Refunds" className={TABLE}>
            <thead className={THEAD}><tr><Th>Refund</Th><Th>Payment</Th><Th>Status</Th><Th>Amount</Th><Th>Reason</Th><Th>When</Th></tr></thead>
            <tbody>{refunds.data.map((r) => <tr key={r.id} className={ROW}><td className="py-1.5 pr-3 font-mono text-xs">{r.id}</td><td className="font-mono text-xs">{r.payment_id}</td><td><Badge status={r.status} /></td><td><Money amount={r.amount} currency={r.currency} /></td><td>{r.reason ?? "-"}</td><td className="text-zinc-400">{when_(r.created_at)}</td></tr>)}</tbody>
          </table>
        )}
      </Panel>
    </div>
  );
}
