import { useState } from "react";
import { archiveTaxRate, deleteProvider, getBillingSettings, listProviders, listTaxRates, listWebhookEvents, putBillingSettings, putProvider, putTaxRate } from "../../api";
import type { BillingSettings, FinalAction, ProviderAccount } from "../../api";
import { Button, ConfirmButton, ErrorView, Loading, Panel, inputCls } from "../../components/ui";
import { useLoad } from "../../hooks";
import { formatBp } from "./money";
import StoreProviders from "./StoreProviders";
import { Badge, BillingError, Empty, Field, Th } from "./shared";
import { FORM_GRID, ROW, TABLE, THEAD, when_ } from "./util";
import { useAct } from "./useAct";
import { Table } from "panelui-web/table";
import { Input } from "panelui-web/input";

const PROVIDERS = ["stripe", "razorpay", "manual"] as const;

/** Provider secrets are write-only: the inputs start empty and a saved account only ever shows its hint. */
function ProviderCard({ name, account, onChanged }: { name: string; account?: ProviderAccount; onChanged: () => void }) {
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
    <section aria-label={`${name} provider`} className="rounded-lg border border-border bg-card p-4">
      <div className="flex items-center justify-between">
        <h3 className="font-medium capitalize">{name}</h3>
        <Badge status={account ? (account.enabled ? "active" : "paused") : "canceled"} />
      </div>
      {account && <p aria-label={`${name} test mode`} className="mt-1 rounded bg-warning/10 px-2 py-1 text-xs text-warning">{account.test_mode ? `Test mode (${account.environment})` : `Mode: ${account.environment}`}</p>}
      {account && <p className="mt-1 text-sm text-muted-foreground">{account.hint ? `Key ${account.hint}` : "Configured"}{account.key_id ? `, key id ${account.key_id}` : ""} · webhook <span className="font-mono text-xs">{account.webhook_path}</span></p>}
      {!account && <p className="mt-1 text-sm text-muted-foreground">Not configured.</p>}
      <form onSubmit={save} className="mt-3 space-y-3">
        {!manual && (
          <div className="grid gap-3 sm:grid-cols-2">
            <Field label={`${name} secret key (write-only)`}><Input size="sm" type="password" autoComplete="off" value={apiKey} onChange={(e) => setApiKey(e.target.value)} placeholder={account ? "leave blank to keep" : name === "stripe" ? "sk_test_..." : "rzp_test_..."} /></Field>
            {name === "razorpay" && <Field label="razorpay key id"><Input size="sm" value={keyId} onChange={(e) => setKeyId(e.target.value)} placeholder={account?.key_id ?? "rzp_test_..."} /></Field>}
            <Field label={`${name} webhook secret (write-only)`}><Input size="sm" type="password" autoComplete="off" value={webhook} onChange={(e) => setWebhook(e.target.value)} placeholder={account ? "leave blank to keep" : "whsec_..."} /></Field>
          </div>
        )}
        <label className="flex items-center gap-2 text-sm text-foreground/80"><input type="checkbox" checked={enabled} onChange={(e) => setEnabled(e.target.checked)} />Enabled</label>
        <ErrorView error={act.error} />
        {act.note && <p role="status" className="text-sm text-success">{act.note}</p>}
        <div className="flex gap-2">
          <Button type="submit" tone="primary" disabled={act.busy}>Save {name}</Button>
          {account && <ConfirmButton label={`Remove ${name}`} disabled={act.busy} onConfirm={() => void act.run(() => deleteProvider(name), "Removed")} />}
        </div>
        <p className="text-xs text-muted-foreground">Test-mode keys only. Secrets are sealed on the server and never shown again; a field left blank keeps its stored value.</p>
      </form>
    </section>
  );
}

function SettingsForm({ initial, onSaved }: { initial: BillingSettings; onSaved: () => void }) {
  const act = useAct(onSaved);
  const [s, setS] = useState(initial);
  const [retry, setRetry] = useState(initial.retry_days.join(", "));
  const [bad, setBad] = useState<string>();
  const save = async (e: React.FormEvent) => {
    e.preventDefault();
    const days = retry.split(",").map((x) => x.trim()).filter(Boolean).map(Number);
    if (days.some((d) => !Number.isInteger(d) || d < 0)) return setBad("Retry days must be whole numbers like 1, 3, 5");
    setBad(undefined);
    await act.run(() => putBillingSettings({ ...s, retry_days: days }), "Settings saved");
  };
  return (
    <form onSubmit={save} aria-label="Billing settings" className="space-y-3">
      <div className={FORM_GRID}>
        <Field label="Default currency"><Input size="sm" maxLength={3} value={s.default_currency} onChange={(e) => setS({ ...s, default_currency: e.target.value.toUpperCase() })} /></Field>
        <Field label="Retry days (comma separated)"><Input size="sm" value={retry} onChange={(e) => setRetry(e.target.value)} /></Field>
        <Field label="After the last retry"><select className={inputCls} value={s.final_action} onChange={(e) => setS({ ...s, final_action: e.target.value as FinalAction })}><option value="cancel">cancel</option><option value="leave_past_due">leave past due</option></select></Field>
        <Field label="Invoice prefix"><Input size="sm" value={s.invoice_prefix} onChange={(e) => setS({ ...s, invoice_prefix: e.target.value })} /></Field>
        <Field label="Credit note prefix"><Input size="sm" value={s.credit_note_prefix} onChange={(e) => setS({ ...s, credit_note_prefix: e.target.value })} /></Field>
      </div>
      {bad && <p role="alert" className="text-sm text-destructive">{bad}</p>}
      <ErrorView error={act.error} />
      {act.note && <p role="status" className="text-sm text-success">{act.note}</p>}
      <Button type="submit" tone="primary" disabled={act.busy}>Save settings</Button>
    </form>
  );
}

function TaxRates() {
  const rates = useLoad(listTaxRates, "tax-rates");
  const act = useAct(rates.reload);
  const [region, setRegion] = useState("");
  const [name, setName] = useState("");
  const [pct, setPct] = useState("");
  const [bad, setBad] = useState<string>();
  const save = async (e: React.FormEvent) => {
    e.preventDefault();
    const m = /^(\d+)(?:\.(\d{1,2}))?$/.exec(pct.trim());
    if (!m) return setBad("Rate must look like 18 or 7.5");
    setBad(undefined);
    await act.run(() => putTaxRate(region, { name, rate_bp: Number(m[1]) * 100 + Number((m[2] ?? "").padEnd(2, "0") || 0), active: true }), "Tax rate saved");
  };
  return (
    <Panel title="Tax rates">
      {rates.data && rates.data.length === 0 && <Empty>No tax rates.</Empty>}
      {rates.data && rates.data.length > 0 && <ul aria-label="Tax rates" className="mb-3 text-sm">{rates.data.map((r) => <li key={r.region}><span className="font-mono text-xs">{r.region}</span> {r.name} {formatBp(r.rate_bp)}{r.active ? <span className="ml-2"><ConfirmButton label={`Archive tax rate ${r.region}`} disabled={act.busy} onConfirm={() => void act.run(() => archiveTaxRate(r.region), "Tax rate archived")} /></span> : <span className="ml-2"><Badge status="canceled" /></span>}</li>)}</ul>}
      <form onSubmit={save} aria-label="New tax rate" className="flex flex-wrap items-end gap-2 text-sm">
        <Field label="Tax region"><Input size="sm" className="w-28" value={region} onChange={(e) => setRegion(e.target.value)} required /></Field>
        <Field label="Tax name"><Input size="sm" className="w-40" value={name} onChange={(e) => setName(e.target.value)} required /></Field>
        <Field label="Rate percent"><Input size="sm" className="w-24" inputMode="decimal" value={pct} onChange={(e) => setPct(e.target.value)} required /></Field>
        <Button type="submit" disabled={act.busy}>Save tax rate</Button>
      </form>
      {bad && <p role="alert" className="mt-2 text-sm text-destructive">{bad}</p>}
      <ErrorView error={act.error} />
    </Panel>
  );
}

export default function Settings() {
  const providers = useLoad(listProviders, "providers");
  const settings = useLoad(getBillingSettings, "billing-settings");
  const events = useLoad((s) => listWebhookEvents(100, s), "webhook-events");
  return (
    <div className="space-y-4">
      <BillingError error={providers.error} />
      {providers.loading && !providers.error && <Loading what="Loading providers" />}
      {providers.data && (
        <div className="grid gap-4 lg:grid-cols-3">
          {PROVIDERS.map((p) => <ProviderCard key={p} name={p} account={providers.data!.find((a) => a.provider === p)} onChanged={providers.reload} />)}
        </div>
      )}
      {providers.data && <StoreProviders accounts={providers.data} onChanged={providers.reload} />}
      <Panel title="Billing settings">
        <ErrorView error={settings.error} />
        {settings.data && <SettingsForm key={JSON.stringify(settings.data)} initial={settings.data} onSaved={settings.reload} />}
      </Panel>
      <TaxRates />
      <Panel title="Webhook events" actions={<Button onClick={events.reload}>Refresh</Button>}>
        <ErrorView error={events.error} />
        {events.data && events.data.length === 0 && <Empty>No webhook deliveries yet.</Empty>}
        {events.data && events.data.length > 0 && (
          <Table aria-label="Webhook events" className={TABLE}>
            <Table.Header className={THEAD}><Table.Row><Th>Received</Th><Th>Provider</Th><Th>Event</Th><Th>Status</Th><Th>Result</Th><Th>Attempts</Th></Table.Row></Table.Header>
            <Table.Body>{events.data.map((e) => <Table.Row key={e.id} className={ROW}><Table.Cell className="py-1.5 pr-3 text-muted-foreground">{when_(e.received_at)}</Table.Cell><Table.Cell>{e.provider}</Table.Cell><Table.Cell><span className="font-mono text-xs">{e.event_type}</span></Table.Cell><Table.Cell><Badge status={e.status} /></Table.Cell><Table.Cell>{e.result ?? "-"}</Table.Cell><Table.Cell>{e.attempts}</Table.Cell></Table.Row>)}</Table.Body>
          </Table>
        )}
      </Panel>
    </div>
  );
}
