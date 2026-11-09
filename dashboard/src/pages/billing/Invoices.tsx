import { useState } from "react";
import { Link, Route, Routes, useParams } from "react-router-dom";
import { INVOICE_STATUSES, creditInvoice, getInvoice, listCreditNotes, listInvoices, markInvoicePaid, payInvoice, voidInvoice } from "../../api";
import type { Invoice, InvoiceStatus } from "../../api";
import { Button, ConfirmButton, ErrorView, Loading, Panel, inputCls } from "../../components/ui";
import { useLoad } from "../../hooks";
import { formatBp, minorToInput, parseMinor } from "./money";
import { Badge, BillingError, Empty, Field, Money, ROW, TABLE, THEAD, Th, idempotencyKey, subjectLink, useAct, when_ } from "./shared";

function CreditNoteForm({ inv, onDone }: { inv: Invoice; onDone: () => void }) {
  const act = useAct(onDone);
  const [amount, setAmount] = useState(minorToInput(inv.amount_paid - inv.amount_refunded, inv.currency));
  const [toBalance, setToBalance] = useState(false);
  const [reason, setReason] = useState("");
  const [bad, setBad] = useState<string>();
  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    const minor = parseMinor(amount, inv.currency);
    if (minor === null || minor <= 0) return setBad(`Enter a positive ${inv.currency} amount`);
    setBad(undefined);
    await act.run(() => creditInvoice(inv.id, { amount: minor, to_balance: toBalance, reason: reason || null, idempotency_key: idempotencyKey("cn") }), toBalance ? "Credited to balance" : "Refund issued");
  };
  return (
    <form onSubmit={submit} aria-label="Credit note" className="space-y-3">
      <div className="grid gap-3 sm:grid-cols-3">
        <Field label={`Credit amount (${inv.currency})`}><input className={inputCls} inputMode="decimal" value={amount} onChange={(e) => setAmount(e.target.value)} /></Field>
        <Field label="Reason"><input className={inputCls} value={reason} onChange={(e) => setReason(e.target.value)} /></Field>
        <label className="flex items-center gap-2 pt-6 text-sm text-zinc-300"><input type="checkbox" checked={toBalance} onChange={(e) => setToBalance(e.target.checked)} />Credit to customer balance instead of refunding</label>
      </div>
      {bad && <p role="alert" className="text-sm text-red-300">{bad}</p>}
      <ErrorView error={act.error} />
      {act.note && <p role="status" className="text-sm text-emerald-300">{act.note}</p>}
      <Button type="submit" disabled={act.busy}>Issue credit note</Button>
    </form>
  );
}

function InvoiceDetail() {
  const { id = "" } = useParams();
  const inv = useLoad((s) => getInvoice(id, s), `invoice:${id}`);
  const notes = useLoad((s) => listCreditNotes(inv.data?.subject, 200, s), `invoice-notes:${inv.data?.subject ?? ""}`);
  const act = useAct(() => { inv.reload(); notes.reload(); });
  const i = inv.data;
  const mine = (notes.data ?? []).filter((n) => n.invoice_id === id);
  return (
    <div className="space-y-4">
      <Link className="text-sm text-amber-300 hover:underline" to="/billing/invoices">Back to invoices</Link>
      <BillingError error={inv.error} />
      {inv.loading && !inv.error && <Loading what="Loading invoice" />}
      {i && (
        <>
          <Panel title={`Invoice ${i.number ?? i.id}`}>
            <dl className="grid gap-3 text-sm sm:grid-cols-4">
              <div><dt className="text-zinc-500">Status</dt><dd><Badge status={i.status} /></dd></div>
              <div><dt className="text-zinc-500">Subject</dt><dd>{subjectLink(i.subject)}</dd></div>
              <div><dt className="text-zinc-500">Reason</dt><dd>{i.billing_reason}</dd></div>
              <div><dt className="text-zinc-500">Attempts</dt><dd>{i.attempt_count}{i.next_attempt_at ? `, next ${when_(i.next_attempt_at)}` : ""}</dd></div>
            </dl>
            <table aria-label="Invoice lines" className={`mt-4 ${TABLE}`}>
              <thead className={THEAD}><tr><Th>Line</Th><Th>Kind</Th><Th>Qty</Th><Th>Unit</Th><Th>Discount</Th><Th>Tax</Th><Th>Amount</Th></tr></thead>
              <tbody>
                {i.lines.map((l) => (
                  <tr key={l.id} className={ROW}>
                    <td className="py-1.5 pr-3">{l.description}</td><td>{l.kind}</td><td>{l.quantity}</td>
                    <td>{l.unit_amount === null ? "-" : <Money amount={l.unit_amount} currency={i.currency} />}</td>
                    <td>{l.discount_amount ? <Money amount={-l.discount_amount} currency={i.currency} /> : "-"}</td>
                    <td>{l.tax_amount ? <><Money amount={l.tax_amount} currency={i.currency} /> <span className="text-zinc-500">({formatBp(l.tax_rate_bp)}{l.tax_inclusive ? ", incl." : ""})</span></> : "-"}</td>
                    <td><Money amount={l.amount} currency={i.currency} /></td>
                  </tr>
                ))}
              </tbody>
            </table>
            <dl aria-label="Invoice totals" className="mt-3 grid max-w-sm grid-cols-2 gap-x-4 gap-y-1 text-sm">
              <dt className="text-zinc-500">Subtotal</dt><dd><Money amount={i.subtotal} currency={i.currency} /></dd>
              <dt className="text-zinc-500">Discount</dt><dd><Money amount={-i.discount} currency={i.currency} /></dd>
              <dt className="text-zinc-500">Tax</dt><dd><Money amount={i.tax} currency={i.currency} /></dd>
              <dt className="font-medium">Total</dt><dd className="font-medium"><Money amount={i.total} currency={i.currency} /></dd>
              <dt className="text-zinc-500">Balance applied</dt><dd><Money amount={i.balance_applied} currency={i.currency} /></dd>
              <dt className="text-zinc-500">Paid</dt><dd><Money amount={i.amount_paid} currency={i.currency} /></dd>
              <dt className="text-zinc-500">Refunded</dt><dd><Money amount={i.amount_refunded} currency={i.currency} /></dd>
              <dt className="text-zinc-500">Due</dt><dd><Money amount={i.amount_due} currency={i.currency} /></dd>
            </dl>
            <p className="mt-2 text-xs text-zinc-500">Lines are frozen when the invoice is finalized: later price changes do not alter them.</p>
          </Panel>
          {(i.status === "open" || i.status === "uncollectible") && (
            <Panel title="Collect">
              <div className="flex flex-wrap gap-2">
                <Button disabled={act.busy} onClick={() => act.run(() => payInvoice(i.id), "Payment attempted")}>Pay now</Button>
                <Button disabled={act.busy} onClick={() => act.run(() => markInvoicePaid(i.id, "manual"), "Marked paid")}>Mark paid (out of band)</Button>
                <ConfirmButton label="Void invoice" disabled={act.busy} onConfirm={() => void act.run(() => voidInvoice(i.id), "Voided")} />
              </div>
              <ErrorView error={act.error} />
              {act.note && <p role="status" className="mt-2 text-sm text-emerald-300">{act.note}</p>}
            </Panel>
          )}
          {i.status === "paid" && <Panel title="Refund or credit"><CreditNoteForm inv={i} onDone={() => { inv.reload(); notes.reload(); }} /></Panel>}
          <Panel title="Credit notes and refunds">
            {mine.length === 0 ? <Empty>No credit notes on this invoice.</Empty> : (
              <table aria-label="Invoice credit notes" className={TABLE}>
                <thead className={THEAD}><tr><Th>Number</Th><Th>Amount</Th><Th>Refunded</Th><Th>To balance</Th><Th>Refund status</Th><Th>Reason</Th></tr></thead>
                <tbody>{mine.map((n) => <tr key={n.id} className={ROW}><td className="py-1.5 pr-3">{n.number}</td><td><Money amount={n.amount} currency={n.currency} /></td><td><Money amount={n.refund_amount} currency={n.currency} /></td><td><Money amount={n.balance_amount} currency={n.currency} /></td><td>{n.refund ? <Badge status={n.refund.status} /> : "-"}</td><td>{n.reason ?? "-"}</td></tr>)}</tbody>
              </table>
            )}
          </Panel>
        </>
      )}
    </div>
  );
}

function InvoiceList() {
  const [status, setStatus] = useState<InvoiceStatus | "">("");
  const [subject, setSubject] = useState("");
  const invoices = useLoad((s) => listInvoices({ status: status || undefined, subject: subject || undefined, limit: 200 }, s), `invoices:${status}:${subject}`);
  const notes = useLoad((s) => listCreditNotes(subject || undefined, 200, s), `credit-notes:${subject}`);
  return (
    <div className="space-y-4">
      <div className="flex flex-wrap items-end gap-3 text-sm">
        <label>Status<select className={`${inputCls} ml-2`} value={status} onChange={(e) => setStatus(e.target.value as InvoiceStatus | "")}><option value="">all</option>{INVOICE_STATUSES.map((s) => <option key={s}>{s}</option>)}</select></label>
        <label>Subject<input className={`${inputCls} ml-2 w-56`} value={subject} onChange={(e) => setSubject(e.target.value)} placeholder="all subjects" /></label>
      </div>
      <BillingError error={invoices.error} />
      {invoices.loading && !invoices.error && <Loading what="Loading invoices" />}
      {invoices.data && invoices.data.length === 0 && <Empty>No invoices match.</Empty>}
      {invoices.data && invoices.data.length > 0 && (
        <table aria-label="Invoices" className={TABLE}>
          <thead className={THEAD}><tr><Th>Invoice</Th><Th>Subject</Th><Th>Status</Th><Th>Total</Th><Th>Due</Th><Th>Refunded</Th><Th>Created</Th></tr></thead>
          <tbody>
            {invoices.data.map((i) => (
              <tr key={i.id} className={ROW}>
                <td className="py-2 pr-3"><Link className="font-mono text-xs text-amber-300 hover:underline" to={`/billing/invoices/${encodeURIComponent(i.id)}`}>{i.number ?? i.id}</Link></td>
                <td>{subjectLink(i.subject)}</td><td><Badge status={i.status} /></td><td><Money amount={i.total} currency={i.currency} /></td>
                <td><Money amount={i.amount_due} currency={i.currency} /></td><td>{i.amount_refunded ? <Money amount={i.amount_refunded} currency={i.currency} /> : "-"}</td>
                <td className="text-zinc-400">{when_(i.created_at)}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
      <Panel title="Credit notes">
        {notes.data && notes.data.length === 0 && <Empty>No credit notes.</Empty>}
        {notes.data && notes.data.length > 0 && (
          <table aria-label="Credit notes" className={TABLE}>
            <thead className={THEAD}><tr><Th>Number</Th><Th>Invoice</Th><Th>Amount</Th><Th>Refunded</Th><Th>To balance</Th><Th>Reason</Th></tr></thead>
            <tbody>{notes.data.map((n) => <tr key={n.id} className={ROW}><td className="py-1.5 pr-3">{n.number}</td><td className="font-mono text-xs">{n.invoice_id}</td><td><Money amount={n.amount} currency={n.currency} /></td><td><Money amount={n.refund_amount} currency={n.currency} /></td><td><Money amount={n.balance_amount} currency={n.currency} /></td><td>{n.reason ?? "-"}</td></tr>)}</tbody>
          </table>
        )}
      </Panel>
    </div>
  );
}

export default function Invoices() {
  return (
    <Routes>
      <Route index element={<InvoiceList />} />
      <Route path=":id" element={<InvoiceDetail />} />
    </Routes>
  );
}
