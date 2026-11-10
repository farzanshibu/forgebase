import { useState } from "react";
import { useSearchParams } from "react-router-dom";
import { creditBalance, getCustomer, listCustomers, listSubscriptions, putCustomer } from "../../api";
import type { Customer } from "../../api";
import { Button, ErrorView, Loading, Panel, inputCls } from "../../components/ui";
import { useLoad } from "../../hooks";
import { Badge, BillingError, Empty, Field, Money, Th } from "./shared";
import { FORM_GRID, ROW, TABLE, THEAD, when_ } from "./util";
import { useAct } from "./useAct";
import { formatCount } from "./money";
import { Table } from "panelui-web/table";
import { Input } from "panelui-web/input";

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
    <Panel title={`Customer ${subject}`}>
      <BillingError error={customer.error} />
      {customer.loading && !customer.error && <Loading what="Loading customer" />}
      {c && form && (
        <div className="space-y-4">
          <dl aria-label="Customer summary" className="grid gap-3 text-sm sm:grid-cols-4">
            <div><dt className="text-muted-foreground">Balance</dt><dd><Money amount={c.balance} currency={c.currency} /></dd></div>
            <div><dt className="text-muted-foreground">Payment method</dt><dd>{c.has_payment_method ? c.payment_method_label ?? "saved" : "none"}</dd></div>
            <div><dt className="text-muted-foreground">Provider</dt><dd>{c.provider ?? "none"}</dd></div>
            <div><dt className="text-muted-foreground">Credits</dt><dd>{credits.data ? formatCount(credits.data.balance) : credits.error ? "n/a" : "…"}</dd></div>
          </dl>
          <form onSubmit={save} aria-label="Edit customer" className="space-y-3">
            <div className={FORM_GRID}>
              <Field label="Billing email"><Input size="sm" value={form.email} onChange={(e) => setDraft({ ...form, email: e.target.value })} /></Field>
              <Field label="Name"><Input size="sm" value={form.name} onChange={(e) => setDraft({ ...form, name: e.target.value })} /></Field>
              <Field label="Tax region"><Input size="sm" value={form.region} onChange={(e) => setDraft({ ...form, region: e.target.value })} placeholder="IN, DE, US-CA" /></Field>
              <Field label="Currency"><Input size="sm" value={form.currency} maxLength={3} onChange={(e) => setDraft({ ...form, currency: e.target.value.toUpperCase() })} /></Field>
              <label className="flex items-center gap-2 text-sm text-foreground/80"><input type="checkbox" checked={form.taxExempt} onChange={(e) => setDraft({ ...form, taxExempt: e.target.checked })} />Tax exempt</label>
            </div>
            <ErrorView error={act.error} />
            {act.note && <p role="status" className="text-sm text-success">{act.note}</p>}
            <Button type="submit" tone="primary" disabled={act.busy}>Save customer</Button>
          </form>
          <div>
            <h3 className="text-xs uppercase text-muted-foreground">Subscriptions</h3>
            {subs.data && subs.data.length === 0 && <Empty>No subscriptions.</Empty>}
            <ul aria-label="Customer subscriptions" className="mt-1 space-y-1 text-sm">
              {(subs.data ?? []).map((s) => <li key={s.id}><Badge status={s.status} /> <span className="font-mono text-xs">{s.items.map((i) => `${i.price}×${i.quantity}`).join(", ")}</span> <span className="text-muted-foreground">renews {when_(s.current_period_end)}</span></li>)}
            </ul>
          </div>
        </div>
      )}
    </Panel>
  );
}

export default function Customers() {
  const [params, setParams] = useSearchParams();
  const subject = params.get("subject") ?? "";
  const [lookup, setLookup] = useState(subject);
  const list = useLoad((s) => listCustomers(500, s), "customers");
  const [filter, setFilter] = useState<"" | "user" | "org">("");
  const rows = (list.data ?? []).filter((c: Customer) => (filter === "org" ? c.subject.startsWith("org:") : filter === "user" ? !c.subject.startsWith("org:") : true));
  return (
    <div className="space-y-4">
      <div className="flex flex-wrap items-end gap-3 text-sm">
        <label>Type
          <select className={`${inputCls} ml-2`} value={filter} onChange={(e) => setFilter(e.target.value as "" | "user" | "org")}>
            <option value="">all</option><option value="user">users</option><option value="org">organizations</option>
          </select>
        </label>
        <form className="flex items-end gap-2" onSubmit={(e) => { e.preventDefault(); setParams(lookup ? { subject: lookup } : {}); }}>
          <label>Subject<Input size="sm" className="ml-2 w-56" value={lookup} onChange={(e) => setLookup(e.target.value)} placeholder="org:acme or a user key" /></label>
          <Button type="submit">Open</Button>
        </form>
      </div>
      <BillingError error={list.error} />
      {list.loading && !list.error && <Loading what="Loading customers" />}
      {list.data && rows.length === 0 && <Empty>No customers match.</Empty>}
      {rows.length > 0 && (
        <Table aria-label="Customers" className={TABLE}>
          <Table.Header className={THEAD}><Table.Row><Th>Subject</Th><Th>Email</Th><Th>Currency</Th><Th>Balance</Th><Th>Payment method</Th><Th>Since</Th></Table.Row></Table.Header>
          <Table.Body>
            {rows.map((c) => (
              <Table.Row key={c.id} className={ROW}>
                <Table.Cell className="py-2 pr-3"><button type="button" className="font-mono text-xs text-primary hover:underline" aria-label={`Open ${c.subject}`} onClick={() => { setLookup(c.subject); setParams({ subject: c.subject }); }}>{c.subject}</button></Table.Cell>
                <Table.Cell>{c.email ?? "-"}</Table.Cell><Table.Cell>{c.currency}</Table.Cell><Table.Cell><Money amount={c.balance} currency={c.currency} /></Table.Cell>
                <Table.Cell>{c.has_payment_method ? c.payment_method_label ?? "saved" : "none"}</Table.Cell><Table.Cell className="text-muted-foreground">{when_(c.created_at)}</Table.Cell>
              </Table.Row>
            ))}
          </Table.Body>
        </Table>
      )}
      {subject && <Detail key={subject} subject={subject} />}
    </div>
  );
}
