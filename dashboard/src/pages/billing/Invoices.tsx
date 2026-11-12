import { useState } from "react";
import { EmptyState } from "../../components/EmptyState";
import { Link, Route, Routes, useParams } from "react-router-dom";
import { INVOICE_STATUSES, creditInvoice, getInvoice, listCreditNotes, listInvoices, markInvoicePaid, payInvoice, voidInvoice } from "../../api";
import type { Invoice, InvoiceStatus } from "../../api";
import { Button, ConfirmButton, ErrorView } from "../../components/ui";
import { Check, Sel } from "../../components/controls";
import { useCrumbs } from "../../components/crumbs";
import { SkeletonLines, SkeletonTable } from "../../components/Skeleton";
import { useLoad } from "../../hooks";
import { formatBp, formatCount, minorToInput, parseMinor } from "./money";
import { BillingError, Field, Filter, Money, None, Note, SubjectLink, TableCard } from "./shared";
import { idempotencyKey, words } from "./util";
import { useAct } from "./useAct";
import { Table } from "panelui-web/table";
import { Input } from "panelui-web/input";
import { Badge } from "../../components/StatusBadge";
import { Th } from "../../components/TableCard";
import { When } from "../../components/Time";

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
    <form onSubmit={submit} aria-label="Credit note" className="space-y-3 p-4">
      <div className="grid gap-3 sm:grid-cols-[12rem_1fr]">
        <Field label={`Credit amount (${inv.currency})`} help={`Up to ${minorToInput(inv.amount_paid - inv.amount_refunded, inv.currency)} is left to refund.`}>
          <Input size="sm" className="tabular-nums" inputMode="decimal" value={amount} onChange={(e) => setAmount(e.target.value)} aria-invalid={bad ? true : undefined} />
        </Field>
        <Field label="Reason" help="Optional. Shown on the credit note."><Input size="sm" value={reason} onChange={(e) => setReason(e.target.value)} placeholder="Goodwill" /></Field>
      </div>
      <Check checked={toBalance} onChange={setToBalance} label="Credit to customer balance instead of refunding" description="The amount is taken off the customer's next invoices; no money goes back to the card." />
      {bad && <p role="alert" className="text-sm text-destructive">{bad}</p>}
      <ErrorView error={act.error} />
      <div className="flex items-center gap-3">
        <Button type="submit" disabled={act.busy}>Issue credit note</Button>
        <Note>{act.note}</Note>
      </div>
    </form>
  );
}

function Totals({ i }: { i: Invoice }) {
  const row = (label: string, amount: number, strong = false) => (
    <>
      <dt className={strong ? "font-semibold text-foreground" : "text-muted-foreground"}>{label}</dt>
      <dd className={`text-right ${strong ? "font-semibold" : ""}`.trim()}><Money amount={amount} currency={i.currency} /></dd>
    </>
  );
  return (
    <dl aria-label="Invoice totals" className="ml-auto grid w-full max-w-xs grid-cols-[1fr_auto] gap-x-6 gap-y-1.5 px-4 py-3 text-sm">
      {row("Subtotal", i.subtotal)}
      {row("Discount", i.discount ? -i.discount : 0)}
      {row("Tax", i.tax)}
      <div className="col-span-2 my-1 border-t border-border" />
      {row("Total", i.total, true)}
      {row("Balance applied", i.balance_applied)}
      {row("Paid", i.amount_paid)}
      {row("Refunded", i.amount_refunded)}
      <div className="col-span-2 my-1 border-t border-border" />
      {row("Amount due", i.amount_due, true)}
    </dl>
  );
}

function InvoiceDetail() {
  const { id = "" } = useParams();
  const inv = useLoad((s) => getInvoice(id, s), `invoice:${id}`);
  const notes = useLoad((s) => listCreditNotes(inv.data?.subject, 200, s), `invoice-notes:${inv.data?.subject ?? ""}`);
  const act = useAct(() => { inv.reload(); notes.reload(); });
  const i = inv.data;
  const mine = (notes.data ?? []).filter((n) => n.invoice_id === id);
  useCrumbs([{ label: "Billing", to: "/billing" }, { label: "Invoices", to: "/billing/invoices" }, { label: i?.number ?? id }]);
  const collect = i && (i.status === "open" || i.status === "uncollectible");
  return (
    <div className="space-y-5">
      <BillingError error={inv.error} />
      {inv.loading && !inv.error && <SkeletonLines lines={5} label="Loading invoice" />}
      {i && (
        <>
          <section aria-label="Invoice summary" className="rounded-xl border border-border bg-card p-5">
            <div className="flex flex-wrap items-start justify-between gap-4">
              <div className="min-w-0">
                <div className="flex flex-wrap items-center gap-2">
                  <h2 className="text-lg font-semibold">Invoice <span className="font-mono">{i.number ?? i.id}</span></h2>
                  <Badge status={i.status} />
                </div>
                <p className="mt-3 text-xs font-medium text-muted-foreground">{i.status === "paid" ? "Total paid" : "Amount due"}</p>
                <p className="text-3xl font-semibold tracking-tight"><Money amount={i.status === "paid" ? i.amount_paid : i.amount_due} currency={i.currency} /></p>
              </div>
              {collect && (
                <div className="flex flex-wrap gap-2">
                  <ConfirmButton label="Void invoice" short="Void invoice" consequence="The customer no longer owes it and it cannot be paid. This cannot be undone." disabled={act.busy} onConfirm={() => void act.run(() => voidInvoice(i.id), "Voided")} />
                  <Button disabled={act.busy} onClick={() => act.run(() => markInvoicePaid(i.id, "manual"), "Marked paid")}>Mark paid (out of band)</Button>
                  <Button tone="primary" disabled={act.busy} onClick={() => act.run(() => payInvoice(i.id), "Payment attempted")}>Pay now</Button>
                </div>
              )}
            </div>
            <dl className="mt-4 grid gap-x-6 gap-y-3 border-t border-border pt-4 text-sm sm:grid-cols-2 lg:grid-cols-4">
              <div><dt className="text-xs text-muted-foreground">Customer</dt><dd className="mt-0.5"><SubjectLink subject={i.subject} /></dd></div>
              <div><dt className="text-xs text-muted-foreground">Reason</dt><dd className="mt-0.5">{words(i.billing_reason)}</dd></div>
              <div><dt className="text-xs text-muted-foreground">Created</dt><dd className="mt-0.5"><When iso={i.created_at} /></dd></div>
              <div><dt className="text-xs text-muted-foreground">Payment attempts</dt><dd className="mt-0.5 tabular-nums">{formatCount(i.attempt_count)}{i.next_attempt_at ? <span className="text-muted-foreground">, next <When iso={i.next_attempt_at} /></span> : ""}</dd></div>
              {i.period_start && <div><dt className="text-xs text-muted-foreground">Period</dt><dd className="mt-0.5"><When iso={i.period_start} day /> to <When iso={i.period_end} day /></dd></div>}
              {i.paid_at && <div><dt className="text-xs text-muted-foreground">Paid</dt><dd className="mt-0.5"><When iso={i.paid_at} /></dd></div>}
              {i.coupon && <div><dt className="text-xs text-muted-foreground">Coupon</dt><dd className="mt-0.5 font-mono text-xs">{i.coupon}</dd></div>}
              {i.tax_region && <div><dt className="text-xs text-muted-foreground">Tax region</dt><dd className="mt-0.5 font-mono text-xs">{i.tax_region}</dd></div>}
            </dl>
            <ErrorView error={act.error} />
            <Note className="mt-3">{act.note}</Note>
          </section>
          <TableCard title="Lines" count={i.lines.length} description="Lines are frozen when the invoice is finalized: later price changes do not alter them.">
            <Table framed aria-label="Invoice lines">
              <Table.Header><Table.Row><Th>Description</Th><Th>Kind</Th><Th align="end">Qty</Th><Th align="end">Unit price</Th><Th align="end">Discount</Th><Th align="end">Tax</Th><Th align="end">Amount</Th></Table.Row></Table.Header>
              <Table.Body>
                {i.lines.map((l) => (
                  <Table.Row key={l.id}>
                    <Table.Cell>{l.description}</Table.Cell>
                    <Table.Cell className="text-muted-foreground">{words(l.kind)}</Table.Cell>
                    <Table.Cell align="end">{formatCount(l.quantity)}</Table.Cell>
                    <Table.Cell align="end">{l.unit_amount === null ? <None /> : <Money amount={l.unit_amount} currency={i.currency} />}</Table.Cell>
                    <Table.Cell align="end">{l.discount_amount ? <Money amount={-l.discount_amount} currency={i.currency} /> : <None />}</Table.Cell>
                    <Table.Cell align="end">{l.tax_amount ? <><Money amount={l.tax_amount} currency={i.currency} /> <span className="block text-xs text-muted-foreground">{formatBp(l.tax_rate_bp)}{l.tax_inclusive ? ", incl." : ""}</span></> : <None />}</Table.Cell>
                    <Table.Cell align="end" className="font-medium"><Money amount={l.amount} currency={i.currency} /></Table.Cell>
                  </Table.Row>
                ))}
              </Table.Body>
            </Table>
            <div className="border-t border-border"><Totals i={i} /></div>
          </TableCard>
          {i.status === "paid" && (
            <TableCard title="Refund or credit" description="Issue a credit note: refund the payment, or credit the customer's balance.">
              <CreditNoteForm inv={i} onDone={() => { inv.reload(); notes.reload(); }} />
            </TableCard>
          )}
          <TableCard title="Credit notes and refunds" count={mine.length}>
            {mine.length === 0 ? <EmptyState compact title="No credit notes on this invoice" why="A credit note reduces what the customer owes, by a refund or a balance credit." /> : (
              <Table framed aria-label="Invoice credit notes">
                <Table.Header><Table.Row><Th>Number</Th><Th align="end">Amount</Th><Th align="end">Refunded</Th><Th align="end">To balance</Th><Th>Refund status</Th><Th>Reason</Th></Table.Row></Table.Header>
                <Table.Body>
                  {mine.map((n) => (
                    <Table.Row key={n.id}>
                      <Table.Cell className="font-mono text-xs">{n.number}</Table.Cell>
                      <Table.Cell align="end"><Money amount={n.amount} currency={n.currency} /></Table.Cell>
                      <Table.Cell align="end"><Money amount={n.refund_amount} currency={n.currency} /></Table.Cell>
                      <Table.Cell align="end"><Money amount={n.balance_amount} currency={n.currency} /></Table.Cell>
                      <Table.Cell>{n.refund ? <Badge status={n.refund.status} /> : <None />}</Table.Cell>
                      <Table.Cell>{n.reason ?? <None />}</Table.Cell>
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

function InvoiceList() {
  const [status, setStatus] = useState<InvoiceStatus | "">("");
  const [subject, setSubject] = useState("");
  const invoices = useLoad((s) => listInvoices({ status: status || undefined, subject: subject || undefined, limit: 200 }, s), `invoices:${status}:${subject}`);
  const notes = useLoad((s) => listCreditNotes(subject || undefined, 200, s), `credit-notes:${subject}`);
  const rows = invoices.data ?? [];
  const filtered = !!(status || subject);
  const toolbar = (
    <>
      <Filter label="Status">
        {(id) => <Sel placeholder="All statuses" id={id} value={status} onChange={(v) => setStatus(v as InvoiceStatus | "")} options={[{ value: "", label: "All statuses" }, ...INVOICE_STATUSES]} className="w-40" />}
      </Filter>
      <Filter label="Subject">
        {(id) => <Input id={id} size="sm" className="w-56 font-mono" value={subject} onChange={(e) => setSubject(e.target.value)} placeholder="All subjects" />}
      </Filter>
    </>
  );
  return (
    <div className="space-y-5">
      <BillingError error={invoices.error} />
      <TableCard title="Invoices" count={invoices.data ? rows.length : undefined} description="Every invoice, newest first. Open one to collect, void or refund it." toolbar={toolbar}>
        {invoices.loading && !invoices.error && <SkeletonTable rows={4} columns={6} label="Loading invoices" />}
        {invoices.data && rows.length === 0 && (
          filtered
            ? <EmptyState compact title="No invoices match" why="Change the status or subject to see more." />
            : <EmptyState title="No invoices yet" why="Invoices are created when a subscription renews or an order is placed." />
        )}
        {rows.length > 0 && (
          <Table framed aria-label="Invoices">
            <Table.Header><Table.Row><Th>Invoice</Th><Th>Customer</Th><Th>Status</Th><Th align="end">Total</Th><Th align="end">Amount due</Th><Th align="end">Refunded</Th><Th>Created</Th></Table.Row></Table.Header>
            <Table.Body>
              {rows.map((i) => (
                <Table.Row key={i.id}>
                  <Table.Cell><Link className="font-mono text-xs font-medium text-primary hover:underline" to={`/billing/invoices/${encodeURIComponent(i.id)}`}>{i.number ?? i.id}</Link></Table.Cell>
                  <Table.Cell><SubjectLink subject={i.subject} /></Table.Cell>
                  <Table.Cell><Badge status={i.status} /></Table.Cell>
                  <Table.Cell align="end" className="font-medium"><Money amount={i.total} currency={i.currency} /></Table.Cell>
                  <Table.Cell align="end">{i.amount_due ? <Money amount={i.amount_due} currency={i.currency} /> : <Money className="text-muted-foreground" amount={0} currency={i.currency} />}</Table.Cell>
                  <Table.Cell align="end">{i.amount_refunded ? <Money amount={i.amount_refunded} currency={i.currency} /> : <None />}</Table.Cell>
                  <Table.Cell className="text-muted-foreground"><When iso={i.created_at} day /></Table.Cell>
                </Table.Row>
              ))}
            </Table.Body>
          </Table>
        )}
      </TableCard>
      <TableCard title="Credit notes" count={notes.data?.length} description="Refunds and balance credits issued against invoices.">
        {notes.data && notes.data.length === 0 && <EmptyState compact title="No credit notes" why="A credit note reduces what the customer owes. Issue one from a paid invoice." />}
        {notes.data && notes.data.length > 0 && (
          <Table framed aria-label="Credit notes">
            <Table.Header><Table.Row><Th>Number</Th><Th>Invoice</Th><Th align="end">Amount</Th><Th align="end">Refunded</Th><Th align="end">To balance</Th><Th>Reason</Th></Table.Row></Table.Header>
            <Table.Body>
              {notes.data.map((n) => (
                <Table.Row key={n.id}>
                  <Table.Cell className="font-mono text-xs">{n.number}</Table.Cell>
                  <Table.Cell><Link className="font-mono text-xs text-primary hover:underline" to={`/billing/invoices/${encodeURIComponent(n.invoice_id)}`}>{n.invoice_id}</Link></Table.Cell>
                  <Table.Cell align="end"><Money amount={n.amount} currency={n.currency} /></Table.Cell>
                  <Table.Cell align="end"><Money amount={n.refund_amount} currency={n.currency} /></Table.Cell>
                  <Table.Cell align="end"><Money amount={n.balance_amount} currency={n.currency} /></Table.Cell>
                  <Table.Cell>{n.reason ?? <None />}</Table.Cell>
                </Table.Row>
              ))}
            </Table.Body>
          </Table>
        )}
      </TableCard>
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
