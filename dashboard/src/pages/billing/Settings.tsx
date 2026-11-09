import { useEffect, useState } from "react";
import { deleteProvider, getBillingSettings, listProviders, listTaxRates, listWebhookEvents, putBillingSettings, putProvider, putTaxRate } from "../../api";
import type { BillingSettings, FinalAction, ProviderAccount } from "../../api";
import { Button, ConfirmButton, ErrorView, Loading, Panel, inputCls } from "../../components/ui";
import { useLoad } from "../../hooks";
import { formatBp } from "./money";
import { Badge, BillingError, Empty, Field, FORM_GRID, ROW, TABLE, THEAD, Th, useAct, when_ } from "./shared";

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
    <section aria-label={`${name} provider`} className="rounded-lg border border-zinc-800 bg-zinc-900/60 p-4">
      <div className="flex items-center justify-between">
        <h3 className="font-medium capitalize">{name}</h3>
        <Badge status={account ? (account.enabled ? "active" : "paused") : "canceled"} />
      </div>
      {account && <p className="mt-1 text-sm text-zinc-400">{account.hint ? `Key ${account.hint}` : "Configured"}{account.key_id ? `, key id ${account.key_id}` : ""} · webhook <span className="font-mono text-xs">{account.webhook_path}</span></p>}
      {!account && <p className="mt-1 text-sm text-zinc-500">Not configured.</p>}
      <form onSubmit={save} className="mt-3 space-y-3">
        {!manual && (
          <div className="grid gap-3 sm:grid-cols-2">
            <Field label={`${name} secret key (write-only)`}><input className={inputCls} type="password" autoComplete="off" value={apiKey} onChange={(e) => setApiKey(e.target.value)} placeholder={account ? "leave blank to keep" : name === "stripe" ? "sk_test_..." : "rzp_test_..."} /></Field>
            {name === "razorpay" && <Field label="razorpay key id"><input className={inputCls} value={keyId} onChange={(e) => setKeyId(e.target.value)} placeholder={account?.key_id ?? "rzp_test_..."} /></Field>}
            <Field label={`${name} webhook secret (write-only)`}><input className={inputCls} type="password" autoComplete="off" value={webhook} onChange={(e) => setWebhook(e.target.value)} placeholder={account ? "leave blank to keep" : "whsec_..."} /></Field>
          </div>
        )}
        <label className="flex items-center gap-2 text-sm text-zinc-300"><input type="checkbox" checked={enabled} onChange={(e) => setEnabled(e.target.checked)} />Enabled</label>
        <ErrorView error={act.error} />
        {act.note && <p role="status" className="text-sm text-emerald-300">{act.note}</p>}
        <div className="flex gap-2">
          <Button type="submit" tone="primary" disabled={act.busy}>Save {name}</Button>
          {account && <ConfirmButton label={`Remove ${name}`} disabled={act.busy} onConfirm={() => void act.run(() => deleteProvider(name), "Removed")} />}
        </div>
        <p className="text-xs text-zinc-500">Test-mode keys only. Secrets are sealed on the server and never shown again.</p>
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
        <Field label="Default currency"><input className={inputCls} maxLength={3} value={s.default_currency} onChange={(e) => setS({ ...s, default_currency: e.target.value.toUpperCase() })} /></Field>
        <Field label="Retry days (comma separated)"><input className={inputCls} value={retry} onChange={(e) => setRetry(e.target.value)} /></Field>
        <Field label="After the last retry"><select className={inputCls} value={s.final_action} onChange={(e) => setS({ ...s, final_action: e.target.value as FinalAction })}><option value="cancel">cancel</option><option value="leave_past_due">leave past due</option></select></Field>
        <Field label="Invoice prefix"><input className={inputCls} value={s.invoice_prefix} onChange={(e) => setS({ ...s, invoice_prefix: e.target.value })} /></Field>
        <Field label="Credit note prefix"><input className={inputCls} value={s.credit_note_prefix} onChange={(e) => setS({ ...s, credit_note_prefix: e.target.value })} /></Field>
      </div>
      {bad && <p role="alert" className="text-sm text-red-300">{bad}</p>}
      <ErrorView error={act.error} />
      {act.note && <p role="status" className="text-sm text-emerald-300">{act.note}</p>}
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
      {rates.data && rates.data.length > 0 && <ul aria-label="Tax rates" className="mb-3 text-sm">{rates.data.map((r) => <li key={r.region}><span className="font-mono text-xs">{r.region}</span> {r.name} {formatBp(r.rate_bp)}</li>)}</ul>}
      <form onSubmit={save} aria-label="New tax rate" className="flex flex-wrap items-end gap-2 text-sm">
        <Field label="Tax region"><input className={`${inputCls} w-28`} value={region} onChange={(e) => setRegion(e.target.value)} required /></Field>
        <Field label="Tax name"><input className={`${inputCls} w-40`} value={name} onChange={(e) => setName(e.target.value)} required /></Field>
        <Field label="Rate percent"><input className={`${inputCls} w-24`} inputMode="decimal" value={pct} onChange={(e) => setPct(e.target.value)} required /></Field>
        <Button type="submit" disabled={act.busy}>Save tax rate</Button>
      </form>
      {bad && <p role="alert" className="mt-2 text-sm text-red-300">{bad}</p>}
      <ErrorView error={act.error} />
    </Panel>
  );
}

export default function Settings() {
  const providers = useLoad(listProviders, "providers");
  const settings = useLoad(getBillingSettings, "billing-settings");
  const events = useLoad((s) => listWebhookEvents(100, s), "webhook-events");
  const [, force] = useState(0);
  useEffect(() => force((v) => v + 1), [settings.data]);
  return (
    <div className="space-y-4">
      <BillingError error={providers.error} />
      {providers.loading && !providers.error && <Loading what="Loading providers" />}
      {providers.data && (
        <div className="grid gap-4 lg:grid-cols-3">
          {PROVIDERS.map((p) => <ProviderCard key={p} name={p} account={providers.data!.find((a) => a.provider === p)} onChanged={providers.reload} />)}
        </div>
      )}
      <Panel title="Billing settings">
        <ErrorView error={settings.error} />
        {settings.data && <SettingsForm key={JSON.stringify(settings.data)} initial={settings.data} onSaved={settings.reload} />}
      </Panel>
      <TaxRates />
      <Panel title="Webhook events" actions={<Button onClick={events.reload}>Refresh</Button>}>
        <ErrorView error={events.error} />
        {events.data && events.data.length === 0 && <Empty>No webhook deliveries yet.</Empty>}
        {events.data && events.data.length > 0 && (
          <table aria-label="Webhook events" className={TABLE}>
            <thead className={THEAD}><tr><Th>Received</Th><Th>Provider</Th><Th>Event</Th><Th>Status</Th><Th>Result</Th><Th>Attempts</Th></tr></thead>
            <tbody>{events.data.map((e) => <tr key={e.id} className={ROW}><td className="py-1.5 pr-3 text-zinc-400">{when_(e.received_at)}</td><td>{e.provider}</td><td><span className="font-mono text-xs">{e.event_type}</span></td><td><Badge status={e.status} /></td><td>{e.result ?? "-"}</td><td>{e.attempts}</td></tr>)}</tbody>
          </table>
        )}
      </Panel>
    </div>
  );
}
