import { useState } from "react";
import { EmptyState } from "../../components/EmptyState";
import { useSearchParams } from "react-router-dom";
import { creditBalance, getCustomer, listCustomers, listSubscriptions, putCustomer } from "../../api";
import type { Customer } from "../../api";
import { Button, ErrorView } from "../../components/ui";
import { Check, Sel } from "../../components/controls";
import { DetailSheet } from "../../components/CreateSheet";
import { SkeletonLines, SkeletonTable } from "../../components/Skeleton";
import { useLoad } from "../../hooks";
import { BillingError, Field, Filter, Money, None, Note, TableCard } from "./shared";
import { FORM_GRID } from "./util";
import { useAct } from "./useAct";
import { formatCount } from "./money";
import { Table } from "panelui-web/table";
import { Input } from "panelui-web/input";
import { Badge } from "../../components/StatusBadge";
import { Stat, StatGrid } from "../../components/Stats";
import { Th } from "../../components/TableCard";
import { When } from "../../components/Time";

const card = (c: Customer) => (c.has_payment_method ? c.payment_method_label ?? "saved" : "none");

function Detail({ subject }: { subject: string }) {
  const customer = useLoad((s) => getCustomer(subject, s), `customer:${subject}`);
  const subs = useLoad((s) => listSubscriptions(subject, s), `cust-subs:${subject}`);
  const credits = useLoad((s) => creditBalance(subject, s), `cust-credits:${subject}`);
  const act = useAct(customer.reload);
  const [draft, setDraft] = useState<{ email: string; name: string; region: string; currency: string; taxExempt: boolean }>();
  const c = customer.data;
  const form = draft ?? (c ? { email: c.email ?? "", name: c.name ?? "", region: c.region ?? "", currency: c.currency, taxExempt: c.tax_exempt } : undefined);
  const save = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!form) return;
    await act.run(() => putCustomer({ subject, email: form.email || null, name: form.name || null, region: form.region || null, currency: form.currency || null, tax_exempt: form.taxExempt }), "Customer saved");
    setDraft(undefined);
  };
  return (
    <div className="space-y-5">
      <BillingError error={customer.error} />
      {customer.loading && !customer.error && <SkeletonLines lines={4} label="Loading customer" />}
      {c && form && (
        <>
          <StatGrid label="Customer summary" columns={2}>
            <Stat label="Balance" value={<Money amount={c.balance} currency={c.currency} />} hint={c.balance < 0 ? "in credit" : c.balance > 0 ? "owed" : undefined} />
            <Stat label="Credits" value={credits.data ? formatCount(credits.data.balance) : credits.error ? "n/a" : "…"} />
            <Stat label="Payment method" value={<span className="text-base">{card(c)}</span>} />
            <Stat label="Provider" value={<span className="text-base">{c.provider ?? "none"}</span>} />
          </StatGrid>
          <section aria-labelledby="cust-subs" className="space-y-2">
            <h3 id="cust-subs" className="text-sm font-semibold">Subscriptions</h3>
            {subs.data && subs.data.length === 0 && <EmptyState compact title="No subscriptions" why="This customer has no subscription. Start one under Subscriptions." />}
            <ul aria-label="Customer subscriptions" className="divide-y divide-border rounded-lg border border-border text-sm empty:hidden">
              {(subs.data ?? []).map((s) => (
                <li key={s.id} className="flex flex-wrap items-center justify-between gap-2 px-3 py-2">
                  <span className="flex items-center gap-2"><Badge status={s.status} /><span className="font-mono text-xs">{s.items.map((i) => `${i.price}×${i.quantity}`).join(", ")}</span></span>
                  <span className="text-xs text-muted-foreground">renews <When iso={s.current_period_end} day /></span>
                </li>
              ))}
            </ul>
          </section>
          <form onSubmit={save} aria-label="Edit customer" className="space-y-3 border-t border-border pt-4">
            <h3 className="text-sm font-semibold">Billing details</h3>
            <div className={FORM_GRID}>
              <Field label="Billing email"><Input size="sm" type="email" value={form.email} onChange={(e) => setDraft({ ...form, email: e.target.value })} /></Field>
              <Field label="Name"><Input size="sm" value={form.name} onChange={(e) => setDraft({ ...form, name: e.target.value })} /></Field>
              <Field label="Tax region" help="Country or region code, such as IN, DE or US-CA."><Input size="sm" className="font-mono" value={form.region} onChange={(e) => setDraft({ ...form, region: e.target.value })} placeholder="IN" /></Field>
              <Field label="Currency"><Input size="sm" className="font-mono" value={form.currency} maxLength={3} onChange={(e) => setDraft({ ...form, currency: e.target.value.toUpperCase() })} /></Field>
            </div>
            <Check checked={form.taxExempt} onChange={(v) => setDraft({ ...form, taxExempt: v })} label="Tax exempt" description="No tax is added to this customer's invoices." />
            <ErrorView error={act.error} />
            <div className="flex items-center gap-3">
              <Button type="submit" tone="primary" disabled={act.busy || !draft}>{act.busy ? "Saving…" : "Save customer"}</Button>
              <Note>{act.note}</Note>
            </div>
          </form>
        </>
      )}
    </div>
  );
}

export default function Customers() {
  const [params, setParams] = useSearchParams();
  const subject = params.get("subject") ?? "";
  const [lookup, setLookup] = useState(subject);
  const list = useLoad((s) => listCustomers(500, s), "customers");
  const [filter, setFilter] = useState<"" | "user" | "org">("");
  const rows = (list.data ?? []).filter((c: Customer) => (filter === "org" ? c.subject.startsWith("org:") : filter === "user" ? !c.subject.startsWith("org:") : true));
  const open = (s: string) => { setLookup(s); setParams(s ? { subject: s } : {}); };
  const toolbar = (
    <>
      <Filter label="Type">
        {(id) => <Sel placeholder="All customers" id={id} value={filter} onChange={(v) => setFilter(v as "" | "user" | "org")} options={[{ value: "", label: "All customers" }, { value: "user", label: "Users" }, { value: "org", label: "Organizations" }]} className="w-40" />}
      </Filter>
      <form className="ml-auto flex items-center gap-2" onSubmit={(e) => { e.preventDefault(); open(lookup.trim()); }}>
        <Filter label="Subject">
          {(id) => <Input id={id} size="sm" className="w-56 font-mono" value={lookup} onChange={(e) => setLookup(e.target.value)} placeholder="org:acme or a user key" />}
        </Filter>
        <Button type="submit" disabled={!lookup.trim()}>Open</Button>
      </form>
    </>
  );
  return (
    <div className="space-y-4">
      <BillingError error={list.error} />
      <TableCard title="Customers" count={list.data ? rows.length : undefined} description="Users and organizations that have a billing record." toolbar={toolbar}>
        {list.loading && !list.error && <SkeletonTable rows={4} columns={5} label="Loading customers" />}
        {list.data && rows.length === 0 && (
          list.data.length === 0
            ? <EmptyState title="No customers yet" why="A customer is created when a user or an organization first subscribes, orders or gets credits." />
            : <EmptyState compact title="No customers match" why="Change the type filter to see more." />
        )}
        {rows.length > 0 && (
          <Table framed aria-label="Customers">
            <Table.Header><Table.Row><Th>Customer</Th><Th>Email</Th><Th>Currency</Th><Th align="end">Balance</Th><Th>Payment method</Th><Th>Since</Th></Table.Row></Table.Header>
            <Table.Body>
              {rows.map((c) => (
                <Table.Row key={c.id} aria-selected={c.subject === subject || undefined}>
                  <Table.Cell>
                    <button type="button" className="max-w-[16rem] truncate font-mono text-xs text-primary hover:underline" title={c.subject} aria-label={`Open ${c.subject}`} onClick={() => open(c.subject)}>{c.subject}</button>
                    {c.name && <span className="block text-xs text-muted-foreground">{c.name}</span>}
                  </Table.Cell>
                  <Table.Cell>{c.email ?? <None />}</Table.Cell>
                  <Table.Cell className="font-mono text-xs">{c.currency}</Table.Cell>
                  <Table.Cell align="end"><Money amount={c.balance} currency={c.currency} /></Table.Cell>
                  <Table.Cell>{c.has_payment_method ? card(c) : <span className="text-muted-foreground">none</span>}</Table.Cell>
                  <Table.Cell className="text-muted-foreground"><When iso={c.created_at} day /></Table.Cell>
                </Table.Row>
              ))}
            </Table.Body>
          </Table>
        )}
      </TableCard>
      <DetailSheet open={!!subject} onOpenChange={(v) => { if (!v) open(""); }} title={<span className="font-mono">{subject}</span>} description="Customer">
        {subject && <Detail key={subject} subject={subject} />}
      </DetailSheet>
    </div>
  );
}
