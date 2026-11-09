import { useState, type ReactNode } from "react";
import { DEFAULT_LDAP, DEFAULT_TURNSTILE, putRealmIdentity, realmInfo, SECRET_MASK, TURNSTILE_ACTIONS, type LdapSettings, type RealmConfig, type RealmIdentity, type TurnstileSettings } from "../../api";
import { Button, ErrorView, Loading, Panel, inputCls } from "../../components/ui";
import { useLoad } from "../../hooks";
import { ActionError, Empty, Field, MultiPick } from "../access/shared";
import { useRun } from "../access/util";
import { Check, Note } from "./shared";
import { csv, useRealm } from "./util";

/** Social/OAuth (read-only), LDAP, SMS (phone) and Turnstile of the selected realm. Secrets are write-only. */
export default function ProvidersPage({ realms, reload }: { realms: RealmIdentity[]; reload: () => void }) {
  const realm = useRealm();
  const ident = realms.find((r) => r.realm === realm);
  if (!ident) return <Empty>Realm {realm} has no identity configuration yet. Create it on the Realms tab.</Empty>;
  return (
    <div className="space-y-6">
      <Social realm={realm} enabled={ident.config.methods.oauth} />
      <Section key={`ldap:${ident.updated_at}`} title="LDAP" ident={ident} reload={reload} render={(cfg, set) => <LdapForm cfg={cfg} set={set} />} />
      <Section key={`sms:${ident.updated_at}`} title="Phone (SMS)" ident={ident} reload={reload} render={(cfg, set) => <SmsForm cfg={cfg} set={set} />} />
      <Section key={`ts:${ident.updated_at}`} title="Turnstile (bot protection)" ident={ident} reload={reload} render={(cfg, set) => <TurnstileForm cfg={cfg} set={set} />} />
    </div>
  );
}

function Social({ realm, enabled }: { realm: string; enabled: boolean }) {
  const l = useLoad((s) => realmInfo(realm, s), `social:${realm}`);
  return (
    <Panel title="Social and OAuth providers">
      <ErrorView error={l.error} />
      {l.loading && !l.error && <Loading what="Loading providers" />}
      {l.data && l.data.oauth_providers.length === 0 && <Empty>{enabled ? "No provider is registered for this realm." : "Social sign-in is off for this realm (enable it under Realms > Settings)."}</Empty>}
      {l.data && l.data.oauth_providers.length > 0 && (
        <ul className="text-sm">{l.data.oauth_providers.map((p) => <li key={p.id} className="border-t border-zinc-800 py-1.5"><span className="font-mono">{p.id}</span> · {p.display_name} <span className="text-xs text-zinc-500">{p.kind}</span></li>)}</ul>
      )}
      <Note>Providers, client ids and secrets are registered in the server's oauth-providers file; the REST API exposes only this read-only list (client secrets never leave the server).</Note>
    </Panel>
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
          {saved && <span role="status" className="text-sm text-emerald-300">Saved</span>}
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
        <input aria-label={label} type="password" autoComplete="off" className={inputCls} value={v} placeholder={isSet ? "set (leave empty to keep)" : "not set"}
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
    <Field label={label}><input aria-label={label} className={`${inputCls} ${mono ? "font-mono" : ""}`} value={(l[key] as string | null) ?? ""} onChange={(e) => up({ [key]: e.target.value } as Partial<LdapSettings>)} /></Field>
  );
  return (
    <>
      <Check label="LDAP sign-in enabled" checked={cfg.methods.ldap} onChange={(v) => set({ ...cfg, methods: { ...cfg.methods, ldap: v }, ldap: cfg.ldap ?? { ...DEFAULT_LDAP } })} />
      <div className="grid gap-3 sm:grid-cols-2">
        {txt("LDAP URL", "url", true)}{txt("Bind DN", "bind_dn", true)}{txt("Base DN", "base_dn", true)}{txt("User filter", "user_filter", true)}
        {txt("Email attribute", "email_attribute")}{txt("Name attribute", "name_attribute")}
        <SecretInput label="Bind password" stored={l.bind_password} onChange={(v) => up({ bind_password: v })} />
        <Field label="Search scope">
          <select aria-label="Search scope" className={inputCls} value={l.scope} onChange={(e) => up({ scope: e.target.value as LdapSettings["scope"] })}>
            <option value="base">base</option><option value="one">one level</option><option value="sub">subtree</option>
          </select>
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
      <Note>The SMS gateway (provider and credentials) is server configuration; the REST API has no endpoint for it, so it cannot be edited here.</Note>
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
        <Field label="Site key"><input aria-label="Site key" className={`${inputCls} font-mono`} value={t.site_key} onChange={(e) => up({ site_key: e.target.value })} /></Field>
        <SecretInput label="Turnstile secret" stored={t.secret} onChange={(v) => up({ secret: v })} />
        <Field label="Allowed hostnames (comma separated)"><input aria-label="Allowed hostnames" className={inputCls} value={t.hostnames.join(", ")} onChange={(e) => up({ hostnames: csv(e.target.value) })} /></Field>
      </div>
      <MultiPick label="Protected actions" options={[...TURNSTILE_ACTIONS]} value={t.actions} onChange={(v) => up({ actions: v as TurnstileSettings["actions"] })} />
    </>
  );
}
