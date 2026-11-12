import { useState, type ReactNode } from "react";
import { EmptyState } from "../../components/EmptyState";
import { archiveTaxRate, deleteProvider, getBillingSettings, listProviders, listTaxRates, listWebhookEvents, putBillingSettings, putProvider, putTaxRate } from "../../api";
import type { BillingSettings, FinalAction, ProviderAccount } from "../../api";
import { Button, ConfirmButton, ErrorView } from "../../components/ui";
import { Sel, Toggle } from "../../components/controls";
import { SkeletonCards, SkeletonTable } from "../../components/Skeleton";
import { useLoad } from "../../hooks";
import { formatBp, formatCount } from "./money";
import StoreProviders from "./StoreProviders";
import { BillingError, Field, FormSection, None, Note, ProviderFacts, ProviderFrame, TableCard } from "./shared";
import { FORM_GRID } from "./util";
import { useAct } from "./useAct";
import { Table } from "panelui-web/table";
import { Input } from "panelui-web/input";
import { Badge } from "../../components/StatusBadge";
import { Th } from "../../components/TableCard";
import { When } from "../../components/Time";

const PROVIDERS = [
  { id: "stripe", title: "Stripe", kind: "Cards and wallets, worldwide" },
  { id: "razorpay", title: "Razorpay", kind: "Cards, UPI and netbanking, India" },
  { id: "manual", title: "Manual", kind: "Bank transfer or cash, marked paid by hand" },
] as const;

/** Provider secrets are write-only: the inputs start empty and a saved account only ever shows its hint. */
function ProviderCard({ name, title, kind, account, onChanged }: { name: string; title: string; kind: string; account?: ProviderAccount; onChanged: () => void }) {
  const act = useAct(onChanged);
  const [apiKey, setApiKey] = useState("");
  const [keyId, setKeyId] = useState("");
  const [webhook, setWebhook] = useState("");
  const [enabled, setEnabled] = useState(account?.enabled ?? true);
  const manual = name === "manual";
  const save = async (e: React.FormEvent) => {
    e.preventDefault();
    const ok = await act.run(() => putProvider(name, { api_key: apiKey || null, key_id: keyId || null, webhook_secret: webhook || null, enabled }), "Saved");
    if (ok) { setApiKey(""); setWebhook(""); }
  };
  return (
    <ProviderFrame label={`${name} provider`} title={title} kind={kind} account={account}>
      <form onSubmit={save} className="flex flex-1 flex-col">
        <div className="flex-1 space-y-3 p-4">
          {account && <p aria-label={`${name} test mode`} className="w-fit rounded-md bg-warning/10 px-2 py-1 text-xs font-medium text-warning">{account.test_mode ? `Test mode (${account.environment})` : `Mode: ${account.environment}`}</p>}
          {account ? <ProviderFacts account={account} /> : <p className="text-sm text-muted-foreground">Not configured.</p>}
          {!manual && (
            <div className="space-y-3">
              <Field label="Secret key">
                <Input size="sm" type="password" aria-label={`${name} secret key (write-only)`} autoComplete="off" value={apiKey} onChange={(e) => setApiKey(e.target.value)} placeholder={account ? "leave blank to keep" : name === "stripe" ? "sk_test_..." : "rzp_test_..."} />
              </Field>
              {name === "razorpay" && <Field label="Key id"><Input size="sm" aria-label="razorpay key id" className="font-mono" value={keyId} onChange={(e) => setKeyId(e.target.value)} placeholder={account?.key_id ?? "rzp_test_..."} /></Field>}
              <Field label="Webhook secret" help={account ? undefined : "From the provider's webhook settings."}>
                <Input size="sm" type="password" aria-label={`${name} webhook secret (write-only)`} autoComplete="off" value={webhook} onChange={(e) => setWebhook(e.target.value)} placeholder={account ? "leave blank to keep" : "whsec_..."} />
              </Field>
            </div>
          )}
          <Toggle checked={enabled} onChange={setEnabled} label="Enabled" description={manual ? "Offer marking invoices paid by hand." : "Charge invoices through this provider."} />
          <ErrorView error={act.error} />
        </div>
        <footer className="flex flex-wrap items-center justify-between gap-2 border-t border-border bg-muted/30 px-4 py-3">
          <span className="flex items-center gap-2">
            {account && <ConfirmButton label={`Remove ${name}`} short="Remove" consequence={`Invoices can no longer be charged through ${title}. Its stored keys are deleted.`} disabled={act.busy} onConfirm={() => void act.run(() => deleteProvider(name), "Removed")} />}
            <Note>{act.note}</Note>
          </span>
          <Button type="submit" tone="primary" aria-label={`Save ${name}`} disabled={act.busy}>{act.busy ? "Saving…" : "Save"}</Button>
        </footer>
      </form>
    </ProviderFrame>
  );
}

function SettingsForm({ initial, onSaved }: { initial: BillingSettings; onSaved: () => void }) {
  const act = useAct(onSaved);
  const [s, setS] = useState(initial);
  const [retry, setRetry] = useState(initial.retry_days.join(", "));
  const days = retry.split(",").map((x) => x.trim()).filter(Boolean);
  const retryBad = days.some((d) => !/^\d+$/.test(d));
  const curBad = !/^[A-Za-z]{3}$/.test(s.default_currency);
  const save = async (e: React.FormEvent) => {
    e.preventDefault();
    if (retryBad || curBad) return;
    await act.run(() => putBillingSettings({ ...s, retry_days: days.map(Number) }), "Settings saved");
  };
  return (
    <form onSubmit={save} aria-label="Billing settings" className="space-y-5 p-4">
      <FormSection title="Currency">
        <Field label="Default currency" help={curBad ? undefined : "Used for new customers and prices without a currency."} className="max-w-48">
          <Input size="sm" className="font-mono uppercase" maxLength={3} value={s.default_currency} onChange={(e) => setS({ ...s, default_currency: e.target.value.toUpperCase() })} aria-invalid={curBad || undefined} />
        </Field>
        {curBad && <p className="text-xs font-medium text-destructive">A three-letter code, like USD.</p>}
      </FormSection>
      <FormSection title="Failed payments" description="When a payment fails, it is retried on these days after the first attempt.">
        <div className={FORM_GRID}>
          <Field label="Retry days" help="Comma separated, like 1, 3, 5.">
            <Input size="sm" className="tabular-nums" value={retry} onChange={(e) => setRetry(e.target.value)} aria-invalid={retryBad || undefined} />
          </Field>
          <Field label="After the last retry">
            <Sel value={s.final_action} onChange={(v) => setS({ ...s, final_action: v as FinalAction })} options={[{ value: "cancel", label: "Cancel the subscription" }, { value: "leave_past_due", label: "Leave it past due" }]} />
          </Field>
        </div>
        {retryBad && <p role="alert" className="text-xs font-medium text-destructive">Retry days must be whole numbers like 1, 3, 5</p>}
      </FormSection>
      <FormSection title="Numbering" description="Prefixes for invoice and credit note numbers. Existing numbers keep theirs.">
        <div className={FORM_GRID}>
          <Field label="Invoice prefix"><Input size="sm" className="font-mono" value={s.invoice_prefix} onChange={(e) => setS({ ...s, invoice_prefix: e.target.value })} /></Field>
          <Field label="Credit note prefix"><Input size="sm" className="font-mono" value={s.credit_note_prefix} onChange={(e) => setS({ ...s, credit_note_prefix: e.target.value })} /></Field>
        </div>
      </FormSection>
      <ErrorView error={act.error} />
      <div className="flex items-center gap-3 border-t border-border pt-4">
        <Button type="submit" tone="primary" disabled={act.busy || retryBad || curBad}>{act.busy ? "Saving…" : "Save settings"}</Button>
        <Note>{act.note}</Note>
      </div>
    </form>
  );
}

function TaxRates() {
  const rates = useLoad(listTaxRates, "tax-rates");
  const act = useAct(rates.reload);
  const [region, setRegion] = useState("");
  const [name, setName] = useState("");
  const [pct, setPct] = useState("");
  const pctBad = pct !== "" && !/^(\d+)(?:\.(\d{1,2}))?$/.test(pct.trim());
  const save = async (e: React.FormEvent) => {
    e.preventDefault();
    const m = /^(\d+)(?:\.(\d{1,2}))?$/.exec(pct.trim());
    if (!m) return;
    const ok = await act.run(() => putTaxRate(region.trim(), { name, rate_bp: Number(m[1]) * 100 + Number((m[2] ?? "").padEnd(2, "0") || 0), active: true }), "Tax rate saved");
    if (ok !== undefined) { setRegion(""); setName(""); setPct(""); }
  };
  return (
    <TableCard title="Tax rates" count={rates.data?.length} description="Tax added to invoices by the customer's tax region. Saving an existing region replaces its rate.">
      {rates.loading && !rates.error && <SkeletonTable rows={2} columns={3} label="Loading tax rates" />}
      <ErrorView error={rates.error} />
      {rates.data && rates.data.length === 0 && <EmptyState compact title="No tax rates" why="Add a rate to charge tax on invoices, such as GST 18% for IN." />}
      {rates.data && rates.data.length > 0 && (
        <ul aria-label="Tax rates" className="divide-y divide-border text-sm">
          {rates.data.map((r) => (
            <li key={r.region} className="grid grid-cols-[5rem_1fr_auto_6rem] items-center gap-3 px-4 py-2.5">
              <span className="font-mono text-xs font-medium">{r.region}</span>
              <span className={r.active ? "" : "text-muted-foreground"}>{r.name} {formatBp(r.rate_bp)}</span>
              <span>{!r.active && <Badge status="archived" />}</span>
              <span className="text-right">{r.active && <ConfirmButton label={`Archive tax rate ${r.region}`} short="Archive" consequence="New invoices for this region are no longer taxed. Past invoices keep their tax." disabled={act.busy} onConfirm={() => void act.run(() => archiveTaxRate(r.region), "Tax rate archived")} />}</span>
            </li>
          ))}
        </ul>
      )}
      <form onSubmit={save} aria-label="New tax rate" className="space-y-2 border-t border-border bg-muted/30 px-4 py-3">
        <div className="flex flex-wrap items-end gap-3">
          <Field label="Tax region" className="w-28"><Input size="sm" className="font-mono uppercase" value={region} onChange={(e) => setRegion(e.target.value)} placeholder="IN" required /></Field>
          <Field label="Tax name" className="w-44"><Input size="sm" value={name} onChange={(e) => setName(e.target.value)} placeholder="GST" required /></Field>
          <Field label="Rate percent" className="w-28"><Input size="sm" className="tabular-nums" inputMode="decimal" value={pct} onChange={(e) => setPct(e.target.value)} placeholder="18" required aria-invalid={pctBad || undefined} /></Field>
          <Button type="submit" disabled={act.busy || pctBad || !region.trim() || !name.trim() || !pct}>Add tax rate</Button>
        </div>
        {pctBad && <p role="alert" className="text-xs font-medium text-destructive">Rate must look like 18 or 7.5</p>}
        <ErrorView error={act.error} />
        <Note>{act.note}</Note>
      </form>
    </TableCard>
  );
}

/** A titled block of the settings page, with an anchor for the jump links. */
function Block({ id, title, description, children }: { id: string; title: string; description: ReactNode; children: ReactNode }) {
  return (
    <section id={id} aria-labelledby={`${id}-h`} className="scroll-mt-4 space-y-3">
      <div>
        <h2 id={`${id}-h`} className="text-base font-semibold tracking-tight">{title}</h2>
        <p className="mt-0.5 text-sm text-muted-foreground">{description}</p>
      </div>
      {children}
    </section>
  );
}

const JUMPS = [["providers", "Payment providers"], ["stores", "Stores and regional"], ["general", "Billing settings"], ["tax", "Tax rates"], ["webhooks", "Webhook events"]] as const;

export default function Settings() {
  const providers = useLoad(listProviders, "providers");
  const settings = useLoad(getBillingSettings, "billing-settings");
  const events = useLoad((s) => listWebhookEvents(100, s), "webhook-events");
  return (
    <div className="space-y-8">
      <nav aria-label="On this page" className="flex flex-wrap gap-x-4 gap-y-1 text-sm">
        {JUMPS.map(([id, label]) => <a key={id} href={`#${id}`} onClick={(e) => { e.preventDefault(); document.getElementById(id)?.scrollIntoView({ behavior: "smooth" }); }} className="text-muted-foreground underline-offset-4 hover:text-foreground hover:underline">{label}</a>)}
      </nav>
      <Block id="providers" title="Payment providers" description="Where invoices are charged. Only test-mode keys are accepted; secrets are sealed on the server and never shown again.">
        <BillingError error={providers.error} />
        {providers.loading && !providers.error && <SkeletonCards count={3} label="Loading providers" />}
        {providers.data && (
          <div className="grid gap-4 md:grid-cols-2 xl:grid-cols-3">
            {PROVIDERS.map((p) => <ProviderCard key={p.id} name={p.id} title={p.title} kind={p.kind} account={providers.data!.find((a) => a.provider === p.id)} onChanged={providers.reload} />)}
          </div>
        )}
      </Block>
      <Block id="stores" title="Stores and regional providers" description="App stores and merchants of record that take the payment themselves, and regional providers. A secret left blank keeps its stored value.">
        {providers.data && <StoreProviders accounts={providers.data} onChanged={providers.reload} />}
      </Block>
      <Block id="general" title="Billing settings" description="Currency, retries for failed payments, and document numbering for the whole instance.">
        <div className="rounded-xl border border-border bg-card">
          <ErrorView error={settings.error} />
          {settings.loading && !settings.error && <div className="p-4"><SkeletonTable rows={2} columns={3} label="Loading settings" /></div>}
          {settings.data && <SettingsForm key={JSON.stringify(settings.data)} initial={settings.data} onSaved={settings.reload} />}
        </div>
      </Block>
      <Block id="tax" title="Tax" description="Rates by region. A customer's tax region decides which rate applies.">
        <TaxRates />
      </Block>
      <Block id="webhooks" title="Webhook events" description="Messages the providers sent to this server, and what it did with them.">
        <TableCard title="Recent events" count={events.data?.length} actions={<Button onClick={events.reload} disabled={events.loading}>Refresh</Button>}>
          <ErrorView error={events.error} />
          {events.loading && !events.error && <SkeletonTable rows={3} columns={5} label="Loading webhook events" />}
          {events.data && events.data.length === 0 && <EmptyState compact title="No webhook deliveries yet" why="Events your payment providers send to this server's webhook URL appear here." />}
          {events.data && events.data.length > 0 && (
            <Table framed aria-label="Webhook events">
              <Table.Header><Table.Row><Th>Received</Th><Th>Provider</Th><Th>Event</Th><Th>Status</Th><Th>Result</Th><Th align="end">Attempts</Th></Table.Row></Table.Header>
              <Table.Body>
                {events.data.map((e) => (
                  <Table.Row key={e.id}>
                    <Table.Cell className="text-muted-foreground"><When iso={e.received_at} /></Table.Cell>
                    <Table.Cell className="capitalize">{e.provider.replace(/_/g, " ")}</Table.Cell>
                    <Table.Cell className="font-mono text-xs">{e.event_type}</Table.Cell>
                    <Table.Cell><Badge status={e.status} /></Table.Cell>
                    <Table.Cell className="max-w-64 truncate" title={e.result ?? undefined}>{e.result ?? <None />}</Table.Cell>
                    <Table.Cell align="end">{formatCount(e.attempts)}</Table.Cell>
                  </Table.Row>
                ))}
              </Table.Body>
            </Table>
          )}
        </TableCard>
      </Block>
    </div>
  );
}
