import { useState, type ReactNode } from "react";
import {
  DEFAULT_LDAP, DEFAULT_TURNSTILE, deleteSmsGateway, deleteSocialProvider, getSmsGateway, listSocialProviders, PROVIDER_KINDS, putRealmIdentity, putSmsGateway, putSocialProvider,
  SECRET_MASK, TURNSTILE_ACTIONS, type LdapSettings, type ProviderKind, type RealmConfig, type RealmIdentity, type SmsConfig, type SmsProviderType, type SocialProvider, type TurnstileSettings,
} from "../../api";
import { Button, ConfirmButton, ErrorView, Loading, Panel } from "../../components/ui";
import { Sel } from "../../components/controls";
import { useLoad } from "../../hooks";
import { ActionError, Empty, Field, MultiPick } from "../access/shared";
import { useRun } from "../access/util";
import { Check, Note } from "./shared";
import { csv, useRealm } from "./util";
import { Input } from "panelui-web/input";

/** Social/OAuth providers (CRUD), LDAP, SMS (phone) with its gateway, and Turnstile of the selected realm. Secrets are write-only. */
export default function ProvidersPage({ realms, reload }: { realms: RealmIdentity[]; reload: () => void }) {
  const realm = useRealm();
  const ident = realms.find((r) => r.realm === realm);
  if (!ident) return <Empty>Realm {realm} has no identity configuration yet. Create it on the Realms tab.</Empty>;
  return (
    <div className="space-y-6">
      <Social realm={realm} enabled={ident.config.methods.oauth} />
      <Section key={`ldap:${ident.updated_at}`} title="LDAP" ident={ident} reload={reload} render={(cfg, set) => <LdapForm cfg={cfg} set={set} />} />
      <Section key={`sms:${ident.updated_at}`} title="Phone (SMS)" ident={ident} reload={reload} render={(cfg, set) => <SmsForm cfg={cfg} set={set} />} />
      <SmsGateway realm={realm} />
      <Section key={`ts:${ident.updated_at}`} title="Turnstile (bot protection)" ident={ident} reload={reload} render={(cfg, set) => <TurnstileForm cfg={cfg} set={set} />} />
    </div>
  );
}

function Social({ realm, enabled }: { realm: string; enabled: boolean }) {
  const l = useLoad((s) => listSocialProviders(realm, s), `social:${realm}`);
  const [editing, setEditing] = useState<SocialProvider | "new">();
  const del = useRun(l.reload);
  return (
    <Panel title="Social and OAuth providers">
      <ErrorView error={l.error} />
      {l.loading && !l.error && <Loading what="Loading providers" />}
      {l.data && l.data.length === 0 && <Empty>{enabled ? "No provider is registered for this realm." : "Social sign-in is off for this realm (enable it under Realms > Settings)."}</Empty>}
      {l.data && l.data.length > 0 && (
        <ul className="text-sm">
          {l.data.map((p) => (
            <li key={p.id} className="flex items-center justify-between border-t border-border py-1.5">
              <span><span className="font-mono">{p.id}</span> · {p.display_name ?? p.id} <span className="text-xs text-muted-foreground">{p.kind} · {p.source === "file" ? "from file (read-only)" : "managed here"}</span></span>
              {!p.read_only && (
                <span className="space-x-2">
                  <Button aria-label={`Edit provider ${p.id}`} onClick={() => setEditing(p)}>Edit</Button>
                  <ConfirmButton label={`Delete provider ${p.id}`} onConfirm={() => void del.run(() => deleteSocialProvider(realm, p.id))} />
                </span>
              )}
            </li>
          ))}
        </ul>
      )}
      <ActionError error={del.error} />
      <div className="mt-3"><Button onClick={() => setEditing("new")}>Add provider</Button></div>
      {editing && <ProviderForm key={editing === "new" ? "new" : editing.id} realm={realm} existing={editing === "new" ? undefined : editing} onDone={() => { setEditing(undefined); l.reload(); }} />}
      <Note>Client secrets are sealed on the server and never returned: leave a secret empty to keep it. Providers defined in the server's oauth-providers file are listed read-only.</Note>
    </Panel>
  );
}

function ProviderForm({ realm, existing, onDone }: { realm: string; existing?: SocialProvider; onDone: () => void }) {
  const [id, setId] = useState(existing?.id ?? "");
  const [kind, setKind] = useState<ProviderKind>(existing?.kind ?? "google");
  const [clientId, setClientId] = useState(existing?.client_id ?? "");
  const [secret, setSecret] = useState<string | null>(existing?.client_secret ?? null);
  const [display, setDisplay] = useState(existing?.display_name ?? "");
  const [issuer, setIssuer] = useState(existing?.issuer ?? "");
  const { run, error, busy } = useRun(onDone);
  return (
    <form aria-label="Social provider" className="mt-3 space-y-3" onSubmit={(e) => {
      e.preventDefault();
      void run(() => putSocialProvider(realm, id, {
        kind, client_id: clientId, client_secret: secret, display_name: display || null, issuer: kind === "oidc" ? issuer || null : null,
        ...(existing?.apple ? { apple: existing.apple } : {}),
      }));
    }}>
      <div className="grid gap-3 sm:grid-cols-2">
        <Field label="Provider id"><Input size="sm" aria-label="Provider id" className="font-mono" value={id} disabled={!!existing} onChange={(e) => setId(e.target.value)} placeholder="google" /></Field>
        <Field label="Kind">
          <Sel aria-label="Provider kind" value={kind} onChange={(v) => setKind(v as ProviderKind)} options={PROVIDER_KINDS} />
        </Field>
        <Field label="Client id"><Input size="sm" aria-label="Client id" className="font-mono" value={clientId} onChange={(e) => setClientId(e.target.value)} /></Field>
        <SecretInput label="Client secret" stored={existing?.client_secret ?? null} onChange={setSecret} />
        <Field label="Display name"><Input size="sm" aria-label="Display name" value={display} onChange={(e) => setDisplay(e.target.value)} /></Field>
        {kind === "oidc" && <Field label="Issuer URL"><Input size="sm" aria-label="Issuer URL" className="font-mono" value={issuer} onChange={(e) => setIssuer(e.target.value)} /></Field>}
      </div>
      <div className="flex items-center gap-3">
        <Button type="submit" tone="primary" disabled={busy || !id || !clientId}>Save provider</Button>
        <Button onClick={onDone}>Cancel</Button>
      </div>
      <ActionError error={error} />
    </form>
  );
}

function SmsGateway({ realm }: { realm: string }) {
  const l = useLoad((s) => getSmsGateway(realm, s), `sms:${realm}:gw`);
  const rm = useRun(l.reload);
  return (
    <Panel title="SMS gateway">
      <ErrorView error={l.error} />
      {l.loading && !l.error && <Loading what="Loading SMS gateway" />}
      {l.data && <GatewayForm key={`${realm}:${l.data.updated_at ?? "none"}`} realm={realm} current={l.data.configured ? l.data.config : null} onSaved={l.reload} />}
      {l.data?.configured && <div className="mt-2"><ConfirmButton label="Remove SMS gateway" onConfirm={() => void rm.run(() => deleteSmsGateway(realm))} /></div>}
      <ActionError error={rm.error} />
    </Panel>
  );
}

function GatewayForm({ realm, current, onSaved }: { realm: string; current: SmsConfig | null; onSaved: () => void }) {
  const [cfg, setCfg] = useState<SmsConfig>(current ?? { provider: "dev" });
  const [saved, setSaved] = useState(false);
  const { run, error, busy } = useRun(() => { setSaved(true); onSaved(); });
  const up = (p: Partial<SmsConfig>) => setCfg({ ...cfg, ...p });
  const txt = (label: string, key: "account_sid" | "from" | "messaging_service_sid" | "base_url" | "url") => (
    <Field label={label}><Input size="sm" aria-label={label} className="font-mono" value={cfg[key] ?? ""} onChange={(e) => up({ [key]: e.target.value } as Partial<SmsConfig>)} /></Field>
  );
  return (
    <form aria-label="SMS gateway" className="space-y-3" onSubmit={(e) => { e.preventDefault(); setSaved(false); void run(() => putSmsGateway(realm, cfg)); }}>
      <p className="text-xs text-muted-foreground">{current ? "This realm sends codes through its own gateway." : "No gateway set for this realm: the instance gateway is used."}</p>
      <Field label="Gateway type">
        <Sel aria-label="Gateway type" value={cfg.provider} onChange={(v) => setCfg({ provider: v as SmsProviderType })} options={[{ value: "dev", label: "dev (writes codes to disk)" }, { value: "twilio", label: "Twilio" }, { value: "webhook", label: "HTTPS webhook" }]} />
      </Field>
      {cfg.provider === "twilio" && (
        <div className="grid gap-3 sm:grid-cols-2">
          {txt("Account SID", "account_sid")}{txt("From number", "from")}{txt("Messaging service SID", "messaging_service_sid")}
          <SecretInput label="Auth token" stored={cfg.auth_token ?? null} onChange={(v) => up({ auth_token: v })} />
        </div>
      )}
      {cfg.provider === "webhook" && (
        <div className="grid gap-3 sm:grid-cols-2">
          {txt("Webhook URL", "url")}
          <SecretInput label="Bearer token" stored={cfg.bearer_token ?? null} onChange={(v) => up({ bearer_token: v })} />
        </div>
      )}
      <div className="flex items-center gap-3">
        <Button type="submit" tone="primary" disabled={busy}>Save SMS gateway</Button>
        {saved && <span role="status" className="text-sm text-success">Saved</span>}
      </div>
      <ActionError error={error} />
    </form>
  );
}

function Section({ title, ident, reload, render }: { title: string; ident: RealmIdentity; reload: () => void; render: (cfg: RealmConfig, set: (c: RealmConfig) => void) => ReactNode }) {
  const [cfg, setCfg] = useState<RealmConfig>(ident.config);
  const [saved, setSaved] = useState(false);
  const { run, error, busy } = useRun(() => { setSaved(true); reload(); });
  return (
    <Panel title={title}>
      <form aria-label={title} className="space-y-3" onSubmit={(e) => { e.preventDefault(); setSaved(false); void run(() => putRealmIdentity(ident.realm, { config: cfg })); }}>
        {render(cfg, setCfg)}
        <div className="flex items-center gap-3">
          <Button type="submit" tone="primary" disabled={busy}>Save {title}</Button>
          {saved && <span role="status" className="text-sm text-success">Saved</span>}
        </div>
        <ActionError error={error} />
      </form>
    </Panel>
  );
}

/** A write-only secret input: shows whether a secret is stored; empty keeps it, typing replaces it, Clear removes it. */
function SecretInput({ label, stored, onChange }: { label: string; stored: string | null; onChange: (v: string | null) => void }) {
  const [v, setV] = useState("");
  const isSet = stored === SECRET_MASK;
  return (
    <Field label={label}>
      <div className="flex items-center gap-2">
        <Input size="sm" aria-label={label} type="password" autoComplete="off" value={v} placeholder={isSet ? "set (leave empty to keep)" : "not set"}
          onChange={(e) => { setV(e.target.value); onChange(e.target.value === "" ? (isSet ? SECRET_MASK : null) : e.target.value); }} />
        {isSet && <Button aria-label={`Clear ${label}`} onClick={() => { setV(""); onChange(""); }}>Clear</Button>}
      </div>
    </Field>
  );
}

function LdapForm({ cfg, set }: { cfg: RealmConfig; set: (c: RealmConfig) => void }) {
  const l: LdapSettings = cfg.ldap ?? DEFAULT_LDAP;
  const up = (p: Partial<LdapSettings>) => set({ ...cfg, ldap: { ...l, ...p } });
  const txt = (label: string, key: keyof LdapSettings, mono = false) => (
    <Field label={label}><Input size="sm" aria-label={label} className={`${mono ? "font-mono" : ""}`} value={(l[key] as string | null) ?? ""} onChange={(e) => up({ [key]: e.target.value } as Partial<LdapSettings>)} /></Field>
  );
  return (
    <>
      <Check label="LDAP sign-in enabled" checked={cfg.methods.ldap} onChange={(v) => set({ ...cfg, methods: { ...cfg.methods, ldap: v }, ldap: cfg.ldap ?? { ...DEFAULT_LDAP } })} />
      <div className="grid gap-3 sm:grid-cols-2">
        {txt("LDAP URL", "url", true)}{txt("Bind DN", "bind_dn", true)}{txt("Base DN", "base_dn", true)}{txt("User filter", "user_filter", true)}
        {txt("Email attribute", "email_attribute")}{txt("Name attribute", "name_attribute")}
        <SecretInput label="Bind password" stored={l.bind_password} onChange={(v) => up({ bind_password: v })} />
        <Field label="Search scope">
          <Sel aria-label="Search scope" value={l.scope} onChange={(v) => up({ scope: v as LdapSettings["scope"] })} options={["base", { value: "one", label: "one level" }, { value: "sub", label: "subtree" }]} />
        </Field>
      </div>
      <div className="flex flex-wrap gap-x-4 gap-y-1">
        <Check label="StartTLS" checked={l.start_tls} onChange={(v) => up({ start_tls: v })} />
        <Check label="Trust directory email" checked={l.trust_email} onChange={(v) => up({ trust_email: v })} />
        <Check label="Link to verified email accounts" checked={l.auto_link_verified_email} onChange={(v) => up({ auto_link_verified_email: v })} />
        <Check label="Allow sign-up from LDAP" checked={l.allow_signup} onChange={(v) => up({ allow_signup: v })} />
      </div>
    </>
  );
}

function SmsForm({ cfg, set }: { cfg: RealmConfig; set: (c: RealmConfig) => void }) {
  return (
    <>
      <Check label="Phone sign-in enabled" checked={cfg.methods.phone} onChange={(v) => set({ ...cfg, methods: { ...cfg.methods, phone: v } })} />
      <Check label="A verified code may create an account" checked={cfg.phone.signup} onChange={(v) => set({ ...cfg, phone: { signup: v } })} />
      <Note>The gateway that delivers the codes is configured in the SMS gateway panel below.</Note>
    </>
  );
}

function TurnstileForm({ cfg, set }: { cfg: RealmConfig; set: (c: RealmConfig) => void }) {
  const t: TurnstileSettings = cfg.turnstile ?? DEFAULT_TURNSTILE;
  const up = (p: Partial<TurnstileSettings>) => set({ ...cfg, turnstile: { ...t, ...p } });
  return (
    <>
      <Check label="Turnstile enabled" checked={t.enabled} onChange={(v) => up({ enabled: v })} />
      <div className="grid gap-3 sm:grid-cols-2">
        <Field label="Site key"><Input size="sm" aria-label="Site key" className="font-mono" value={t.site_key} onChange={(e) => up({ site_key: e.target.value })} /></Field>
        <SecretInput label="Turnstile secret" stored={t.secret} onChange={(v) => up({ secret: v })} />
        <Field label="Allowed hostnames (comma separated)"><Input size="sm" aria-label="Allowed hostnames" value={t.hostnames.join(", ")} onChange={(e) => up({ hostnames: csv(e.target.value) })} /></Field>
      </div>
      <MultiPick label="Protected actions" options={[...TURNSTILE_ACTIONS]} value={t.actions} onChange={(v) => up({ actions: v as TurnstileSettings["actions"] })} />
    </>
  );
}
