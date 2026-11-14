import { useState } from "react";
import { Input } from "panelui-web/input";
import { ApiError, deleteMailProvider, getMailProvider, INSTANCE_REALM, MAIL_PROVIDER_TYPES, putMailProvider, sendMailProviderTest } from "../../api";
import type { MailProvider, MailProviderType, MailTestResult } from "../../api";
import { Button, ConfirmButton } from "../../components/ui";
import { FormField } from "../../components/FormField";
import { Sel } from "../../components/controls";
import { SkeletonLines } from "../../components/Skeleton";
import { Badge } from "../../components/StatusBadge";
import { useLoad } from "../../hooks";
import { notify } from "../../toaster";
import { ActionError } from "../access/shared";
import { TableCard } from "../billing/shared";
import { Callout } from "../jobs/ops";
import { Field, SecretInput } from "./shared";
import { MAIL_FIELDS, MAIL_PROVIDER_LABEL, checkMail, isEmail, mailBody } from "./gatewayFields";
import type { GatewayField } from "./gatewayFields";
import { useRealm } from "./util";

type Values = Record<string, string | null>;

const initial = (c: MailProvider["config"]): { provider: MailProviderType; values: Values; from: string } => {
  const values: Values = {};
  for (const [k, v] of Object.entries(c ?? {})) if (k !== "provider" && k !== "from_address") values[k] = v == null ? null : String(v);
  return { provider: c?.provider ?? "smtp", values, from: c?.from_address ?? "" };
};

/** A text, select or secret input for one field of a provider; its error shows under it. The label stays the control's whole name. */
export function GatewayFieldInput({ f, value, error, stored, onChange }: { f: GatewayField<string>; value: string | null | undefined; error?: string; stored?: string | null; onChange: (v: string | null) => void }) {
  const note = (
    <>
      {error && <span role="alert" className="block font-medium text-destructive">{error}</span>}
      {f.required && !f.secret && <span className="block">Required.</span>}
      {f.help}
    </>
  );
  if (f.secret) {
    return (
      <div className="flex flex-col gap-1">
        <SecretInput label={f.label} stored={stored} onChange={onChange} />
        {(f.help || error || f.required) && <p className="text-xs text-muted-foreground">{error && <span role="alert" className="block font-medium text-destructive">{error}</span>}{f.required && !error && "Required. "}{f.help}</p>}
      </div>
    );
  }
  return (
    <Field label={f.label} help={note}>
      {f.options
        ? <Sel aria-label={f.label} className="w-full" value={value ?? ""} onChange={(v) => onChange(v || null)} options={[{ value: "", label: "Default" }, ...f.options]} />
        : <Input size="sm" aria-label={f.label} aria-invalid={error ? true : undefined} className={f.kind === "url" || f.kind === "port" ? "font-mono" : undefined} inputMode={f.kind === "port" ? "numeric" : undefined} placeholder={f.placeholder} value={value ?? ""} onChange={(e) => onChange(e.target.value)} />}
    </Field>
  );
}

function Form({ scope, data, onSaved }: { scope: string; data: MailProvider; onSaved: () => void }) {
  const start = initial(data.config);
  const [provider, setProvider] = useState<MailProviderType>(start.provider);
  const [values, setValues] = useState<Values>(start.values);
  const [from, setFrom] = useState(start.from);
  const [touched, setTouched] = useState(false);
  const [error, setError] = useState<unknown>();
  const [busy, setBusy] = useState(false);
  const errors = checkMail(provider, values, from);
  const shown = touched ? errors : {};
  const same = data.config?.provider === provider;
  const set = (k: string, v: string | null) => { setValues((x) => ({ ...x, [k]: v })); setTouched(true); };

  async function save(e: React.FormEvent) {
    e.preventDefault();
    setTouched(true);
    if (Object.keys(errors).length) return;
    setBusy(true);
    setError(undefined);
    try {
      await putMailProvider(mailBody(provider, values, from), scope);
      notify.saved(`Mail provider for ${scope === INSTANCE_REALM ? "the instance" : scope}`);
      onSaved();
    } catch (err) {
      setError(err);
    } finally {
      setBusy(false);
    }
  }
  return (
    <form aria-label="Mail provider" className="space-y-4 p-4" onSubmit={(e) => void save(e)} noValidate>
      <FormField label="Provider" help={provider === "log" ? "Nothing is sent: each message is only written to the server log. For development." : "Where this configuration sends email through."}>
        {(p) => <Sel id={p.id} aria-label="Provider" className="w-full max-w-sm" value={provider} onChange={(v) => { setProvider(v as MailProviderType); setValues({}); }} options={MAIL_PROVIDER_TYPES.map((t) => ({ value: t, label: MAIL_PROVIDER_LABEL[t] }))} />}
      </FormField>
      <div className="grid gap-3 sm:grid-cols-2">
        {MAIL_FIELDS[provider].map((f) => (
          <GatewayFieldInput key={`${provider}:${f.key}`} f={f} value={values[f.key]} stored={same ? start.values[f.key] : null} error={shown[f.key]} onChange={(v) => set(f.key, v)} />
        ))}
        <FormField label="From address" help="The From of every message. Empty uses the server's default." error={shown.from_address} className="sm:col-span-2">
          {(p) => <Input {...p} size="sm" className="font-mono" placeholder="no-reply@example.com" value={from} onChange={(e) => { setFrom(e.target.value); setTouched(true); }} />}
        </FormField>
      </div>
      {!same && data.configured && <p role="note" className="text-xs text-warning">Changing the provider drops the stored secrets of the old one: enter the new provider's secrets.</p>}
      <ActionError error={error} />
      <div className="flex flex-wrap items-center gap-3 border-t border-border pt-4">
        <Button type="submit" tone="primary" disabled={busy}>{busy ? "Saving…" : "Save mail provider"}</Button>
        {touched && Object.keys(errors).length > 0 && <span role="alert" className="text-xs text-destructive">Fix the fields marked above to save.</span>}
        <span className="text-xs text-muted-foreground">Saving checks the settings; it sends nothing. Use Send test email to try it.</span>
      </div>
    </form>
  );
}

/** One real message through the provider the scope would use now (not queued, not logged). */
function TestSend({ scope }: { scope: string }) {
  const [to, setTo] = useState("");
  const [result, setResult] = useState<MailTestResult>();
  const [fail, setFail] = useState<string>();
  const [error, setError] = useState<unknown>();
  const [busy, setBusy] = useState(false);
  const bad = to.trim() !== "" && !isEmail(to);
  async function send(e: React.FormEvent) {
    e.preventDefault();
    setResult(undefined); setFail(undefined); setError(undefined); setBusy(true);
    try {
      setResult(await sendMailProviderTest(to.trim(), scope));
    } catch (err) {
      if (err instanceof ApiError && err.code === "mail_test_failed") setFail(err.message);
      else setError(err);
    } finally {
      setBusy(false);
    }
  }
  return (
    <section aria-label="Send test email" className="space-y-3 border-t border-border bg-muted/20 p-4">
      <h3 className="text-sm font-semibold">Send test email</h3>
      <p className="text-xs text-muted-foreground">Sends one message now through the provider this scope uses, so you see the provider's real answer. Save your changes first.</p>
      <form className="flex flex-wrap items-end gap-3" onSubmit={(e) => void send(e)}>
        <FormField label="Send to" error={bad ? "Use an email address such as you@example.com." : undefined} className="w-72">
          {(p) => <Input {...p} size="sm" type="email" placeholder="you@example.com" value={to} onChange={(e) => setTo(e.target.value)} />}
        </FormField>
        <Button type="submit" disabled={busy || !to.trim() || bad}>{busy ? "Sending…" : "Send test email"}</Button>
      </form>
      {result && <Callout tone="ok" role="status" title="Test email accepted"><>Sent through <span className="font-mono">{result.provider}</span> from <span className="font-mono">{result.from}</span>. Provider message id: <span className="font-mono">{result.message_id ?? "none returned"}</span>.</></Callout>}
      {fail && <Callout tone="bad" role="alert" title="The provider did not accept the test message">{fail}</Callout>}
      <ActionError error={error} />
    </section>
  );
}

/** The email provider of a realm, or of the instance (`*`): which transport sends transactional email, its settings and a test send (ADR-0039). Platform administrators only. */
export default function EmailProviderPage() {
  const realm = useRealm();
  const [scopeKind, setScopeKind] = useState<"realm" | "instance">("realm");
  const scope = scopeKind === "instance" ? INSTANCE_REALM : realm;
  const l = useLoad((s) => getMailProvider(scope, s), `mailprov:${scope}`);
  return (
    <div className="space-y-6">
      <TableCard
        title="Email provider"
        description={scope === INSTANCE_REALM ? "The instance's transport: realms without their own provider use it." : <>The transport realm <span className="font-mono">{realm}</span> sends through. Without one it uses the instance's provider.</>}
        actions={l.data?.configured ? <Badge tone="ok" dot>configured</Badge> : <Badge tone="neutral" dot>not configured</Badge>}
        toolbar={
          <div className="flex items-center gap-2">
            <label htmlFor="mail-scope" className="whitespace-nowrap text-xs font-medium text-muted-foreground">Configure</label>
            <Sel id="mail-scope" aria-label="Configure" className="w-64" value={scopeKind} onChange={(v) => setScopeKind(v as "realm" | "instance")} options={[{ value: "realm", label: `Realm ${realm}` }, { value: "instance", label: "Instance default (*)" }]} />
          </div>
        }
      >
        <ActionError error={l.error} />
        {l.loading && !l.data && !l.error && <div className="p-4"><SkeletonLines lines={4} label="Loading the mail provider" /></div>}
        {l.data && !l.data.configured && (
          <p role="note" className="mx-4 mt-4 rounded-lg border border-border bg-muted/40 p-3 text-sm text-muted-foreground">
            {scope === INSTANCE_REALM ? "No instance provider is set: mail falls back to the log provider, which sends nothing." : "This realm has no provider of its own: it uses the instance's provider, which falls back to the log provider (nothing is sent) when none is set."}
          </p>
        )}
        {l.data && <Form key={`${scope}:${l.data.updated_at ?? "none"}`} scope={scope} data={l.data} onSaved={l.reload} />}
        {l.data?.configured && (
          <div className="flex justify-end border-t border-border px-4 py-3">
            <ConfirmButton label="Remove mail provider" short="Remove provider" consequence={scope === INSTANCE_REALM ? "The instance goes back to the log provider: nothing is sent until you set another." : "This realm uses the instance's provider again."} onConfirm={async () => { await deleteMailProvider(scope); notify.deleted("Mail provider"); l.reload(); }} />
          </div>
        )}
        <TestSend scope={scope} />
      </TableCard>
    </div>
  );
}
