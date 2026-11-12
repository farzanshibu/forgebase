import { Textarea } from "panelui-web/textarea";
import { useState } from "react";
import { deleteProvider, putProvider } from "../../api";
import type { ProviderAccount } from "../../api";
import { Button, ConfirmButton, ErrorView } from "../../components/ui";
import { Sel, Toggle } from "../../components/controls";
import { Field, Note, ProviderFacts, ProviderFrame } from "./shared";
import { useAct } from "./useAct";
import { Input } from "panelui-web/input";

type Where = "top" | "config";
interface Spec {
  /** Field name on the wire. */
  name: string;
  label: string;
  where: Where;
  secret?: boolean;
  multiline?: boolean;
  options?: string[];
  placeholder?: string;
  /** `platform_keys`: a JSON array, sent as an array. */
  json?: boolean;
  /** A line of help under the field. */
  help?: string;
}
interface Card { id: string; title: string; kind: "store" | "payment"; test: string; fields: Spec[]; apiBase?: "required" | "optional" }

/** Provider forms of FB-72 (ADR-0021 section 9). Every secret and every `config` value is sealed on the server and never returned. */
const CARDS: Card[] = [
  {
    id: "app_store", title: "App Store", kind: "store",
    test: "Sandbox only: environment sandbox, xcode or local_testing. The production API base and Production transactions are refused.",
    fields: [
      { name: "issuer_id", label: "Issuer id", where: "config" }, { name: "key_id", label: "App Store Connect key id", where: "config" },
      { name: "bundle_id", label: "Bundle id", where: "config" }, { name: "environment", label: "Environment", where: "config", options: ["sandbox", "xcode", "local_testing"] },
      { name: "private_key", label: "App Store Connect private key (write-only)", where: "config", secret: true, multiline: true, placeholder: "-----BEGIN PRIVATE KEY-----" },
    ],
  },
  {
    id: "google_play", title: "Google Play", kind: "store",
    test: "Test purchases only: subscriptions without testPurchase and products with purchaseType other than 0 (licence tester) are refused.",
    fields: [
      { name: "package_name", label: "Package name", where: "config" }, { name: "push_audience", label: "Push audience", where: "config" },
      { name: "push_service_account", label: "Push service account email", where: "config" },
      { name: "service_account", label: "Service account JSON (write-only)", where: "config", secret: true, multiline: true, placeholder: '{"type": "service_account", ...}' },
    ],
  },
  {
    id: "creem", title: "Creem", kind: "store",
    test: "Test mode only: creem_test_ keys and test-api.creem.io; production payloads are refused.",
    apiBase: "optional",
    fields: [
      { name: "api_key", label: "Creem API key (write-only)", where: "top", secret: true, placeholder: "creem_test_..." },
      { name: "webhook_secret", label: "Creem webhook secret (write-only)", where: "top", secret: true },
    ],
  },
  {
    id: "wechat_pay", title: "WeChat Pay", kind: "payment",
    test: "WeChat Pay has no test keys: a sandbox or mock API base is required and the production hosts are refused.",
    apiBase: "required",
    fields: [
      { name: "key_id", label: "Merchant id (mchid)", where: "top" }, { name: "api_key", label: "APIv3 key, 32 bytes (write-only)", where: "top", secret: true },
      { name: "appid", label: "App id", where: "config" }, { name: "merchant_serial", label: "Merchant certificate serial", where: "config" },
      { name: "notify_url", label: "Notify URL", where: "config" },
      { name: "private_key", label: "Merchant private key (write-only)", where: "config", secret: true, multiline: true },
      { name: "platform_keys", label: 'Platform keys (write-only, JSON [{"serial", "pem"}])', where: "config", secret: true, multiline: true, json: true, help: 'A JSON array like [{"serial": "…", "pem": "…"}].' },
    ],
  },
];

function ProviderForm({ card, account, onChanged }: { card: Card; account?: ProviderAccount; onChanged: () => void }) {
  const act = useAct(onChanged);
  const [values, setValues] = useState<Record<string, string>>({});
  const [apiBase, setApiBase] = useState("");
  // Non-secret values come back from the server; secrets never do, so a stored one shows "leave blank to keep".
  const shown = (f: Spec): string => {
    const v = f.where === "top" ? (f.name === "key_id" ? account?.key_id : undefined) : account?.config?.[f.name];
    return typeof v === "string" ? v : "";
  };
  const isSet = (f: Spec) => !!account?.secrets_set.includes(f.where === "config" ? `config.${f.name}` : f.name);
  const [enabled, setEnabled] = useState(account?.enabled ?? true);
  const [bad, setBad] = useState<string>();
  const set = (k: string, v: string) => setValues((s) => ({ ...s, [k]: v }));
  const save = async (e: React.FormEvent) => {
    e.preventDefault();
    const top: Record<string, string | null> = {};
    const config: Record<string, unknown> = {};
    for (const f of card.fields) {
      const raw = (values[f.name] ?? (f.secret ? "" : shown(f) || f.options?.[0]) ?? "").trim();
      // A blank secret is not sent: the server keeps the stored one.
      if (!raw && f.secret) continue;
      let v: unknown = raw;
      if (f.json) {
        try { v = JSON.parse(raw); } catch { return setBad(`${f.name} must be valid JSON`); }
        if (!Array.isArray(v)) return setBad(`${f.name} must be a JSON array`);
      }
      if (f.where === "top") top[f.name] = raw;
      else config[f.name] = v;
    }
    if (card.apiBase === "required" && !apiBase.trim()) return setBad("API base is required: this provider has no test keys");
    setBad(undefined);
    const hasConfig = Object.keys(config).length > 0;
    const ok = await act.run(
      () => putProvider(card.id, { ...top, api_base: apiBase.trim() || null, config: hasConfig ? config : undefined, enabled }),
      "Saved",
    );
    // Secrets never stay in the page after a save.
    if (ok) setValues((s) => Object.fromEntries(Object.entries(s).filter(([k]) => !card.fields.find((f) => f.name === k)?.secret)));
  };
  const keep = (f: Spec) => f.secret && isSet(f);
  return (
    <ProviderFrame label={`${card.id} provider`} title={card.title} kind={card.kind === "store" ? "Merchant of record" : "Payment provider"} account={account}>
      <form onSubmit={save} className="flex flex-1 flex-col">
        <div className="flex-1 space-y-3 p-4">
          <p aria-label={`${card.id} test mode`} className="rounded-md bg-warning/10 px-2 py-1.5 text-xs text-warning">
            <span className="font-medium">Test mode{account ? ` (${account.environment})` : ""}.</span> {card.test}
          </p>
          {account && <ProviderFacts account={account} hook={card.kind === "store" ? "Notification URL" : "Webhook URL"} />}
          <div className="grid gap-3 sm:grid-cols-2">
            {card.fields.map((f) => (
              <Field key={f.name} label={f.label.replace(/ \(write-only.*\)$/, "")} help={[f.help, keep(f) ? "Stored. Leave blank to keep it." : f.secret ? "Write-only: never shown again." : undefined].filter(Boolean).join(" ") || undefined} className={f.multiline ? "sm:col-span-2" : ""}>
                {f.options ? (
                  <Sel aria-label={`${card.id} ${f.label}`} value={values[f.name] ?? (shown(f) || f.options[0]) ?? ""} onChange={(v) => set(f.name, v)} options={f.options} />
                ) : f.multiline ? (
                  <Textarea aria-label={`${card.id} ${f.label}`} minRows={4} className="font-mono text-xs" autoComplete="off" spellCheck={false} value={values[f.name] ?? shown(f)} onChange={(e) => set(f.name, e.target.value)} placeholder={keep(f) ? "leave blank to keep" : f.placeholder} required={!keep(f)} />
                ) : (
                  <Input size="sm" aria-label={`${card.id} ${f.label}`} className={f.secret ? "" : "font-mono"} type={f.secret ? "password" : "text"} autoComplete="off" value={values[f.name] ?? shown(f)} onChange={(e) => set(f.name, e.target.value)} placeholder={keep(f) ? "leave blank to keep" : f.placeholder} required={!keep(f)} />
                )}
              </Field>
            ))}
            {card.apiBase && (
              <Field label={`API base${card.apiBase === "required" ? "" : " (optional)"}`} help={card.apiBase === "required" ? "Required: a sandbox or mock host." : undefined} className="sm:col-span-2">
                <Input size="sm" aria-label={`${card.id} API base${card.apiBase === "required" ? "" : " (optional)"}`} className="font-mono" value={apiBase} onChange={(e) => setApiBase(e.target.value)} placeholder={account?.api_base ?? "https://..."} />
              </Field>
            )}
          </div>
          <Toggle checked={enabled} onChange={setEnabled} label="Enabled" />
          {bad && <p role="alert" className="text-sm font-medium text-destructive">{bad}</p>}
          <ErrorView error={act.error} />
        </div>
        <footer className="flex flex-wrap items-center justify-between gap-2 border-t border-border bg-muted/30 px-4 py-3">
          <span className="flex items-center gap-2">
            {account && <ConfirmButton label={`Remove ${card.id}`} short="Remove" consequence={`${card.title} purchases are no longer verified. Its stored credentials are deleted.`} disabled={act.busy} onConfirm={() => void act.run(() => deleteProvider(card.id), "Removed")} />}
            <Note>{act.note}</Note>
          </span>
          <Button type="submit" tone="primary" aria-label={`Save ${card.id}`} disabled={act.busy}>{act.busy ? "Saving…" : "Save"}</Button>
        </footer>
      </form>
    </ProviderFrame>
  );
}

export default function StoreProviders({ accounts, onChanged }: { accounts: ProviderAccount[]; onChanged: () => void }) {
  return (
    <div role="group" aria-label="Store and regional providers" className="grid gap-4 xl:grid-cols-2">
      {CARDS.map((c) => <ProviderForm key={c.id} card={c} account={accounts.find((a) => a.provider === c.id)} onChanged={onChanged} />)}
    </div>
  );
}
