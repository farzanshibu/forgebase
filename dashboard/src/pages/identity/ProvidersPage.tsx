import { useState, type ReactNode } from "react";
import { EmptyState } from "../../components/EmptyState";
import {
  DEFAULT_LDAP, DEFAULT_TURNSTILE, deleteSmsGateway, deleteSocialProvider, getSmsGateway, listSocialProviders, PROVIDER_KINDS, putRealmIdentity, putSmsGateway, putSocialProvider,
  SECRET_MASK, TURNSTILE_ACTIONS, type LdapSettings, type ProviderKind, type RealmConfig, type RealmIdentity, type SmsConfig, type SmsProviderType, type SocialProvider, type TurnstileSettings,
} from "../../api";
import { Button, ConfirmButton } from "../../components/ui";
import { Sel, Toggle } from "../../components/controls";
import { CreateSheet } from "../../components/CreateSheet";
import { SkeletonLines } from "../../components/Skeleton";
import { useLoad } from "../../hooks";
import { notify } from "../../toaster";
import { ActionError } from "../access/shared";
import { useRun } from "../access/util";
import { TableCard, Tag } from "../billing/shared";
import { Check, Field, MultiPick, Note, Saved } from "./shared";
import { csv, useRealm } from "./util";
import { Input } from "panelui-web/input";
import { Badge } from "../../components/StatusBadge";

/** Social/OAuth providers (CRUD), LDAP, SMS (phone) with its gateway, and Turnstile of the selected realm. Secrets are write-only. */
export default function ProvidersPage({ realms, reload }: { realms: RealmIdentity[]; reload: () => void }) {
  const realm = useRealm();
  const ident = realms.find((r) => r.realm === realm);
  if (!ident) return <EmptyState title={`Realm ${realm} has no identity configuration yet`} why="Create it on the Realms tab, then set up its providers here." />;
  const on = (v: boolean) => <Badge status={v ? "enabled" : "off"}>{v ? "on" : "off"}</Badge>;
  return (
    <div className="space-y-6">
      <Social realm={realm} enabled={ident.config.methods.oauth} />
      <div className="grid gap-6 xl:grid-cols-2">
        <Section key={`ldap:${ident.updated_at}`} title="LDAP" description="Sign in with a company directory such as Active Directory or OpenLDAP." badge={on(ident.config.methods.ldap)} ident={ident} reload={reload} render={(cfg, set) => <LdapForm cfg={cfg} set={set} />} />
        <div className="space-y-6">
          <Section key={`sms:${ident.updated_at}`} title="Phone (SMS)" description="Sign in with a one-time code sent by text message." badge={on(ident.config.methods.phone)} ident={ident} reload={reload} render={(cfg, set) => <SmsForm cfg={cfg} set={set} />} />
          <SmsGateway realm={realm} />
        </div>
      </div>
      <Section key={`ts:${ident.updated_at}`} title="Turnstile (bot protection)" description="Cloudflare Turnstile challenges on sign-up, sign-in and other actions you choose." badge={on(!!ident.config.turnstile?.enabled)} ident={ident} reload={reload} render={(cfg, set) => <TurnstileForm cfg={cfg} set={set} />} />
    </div>
  );
}

function Social({ realm, enabled }: { realm: string; enabled: boolean }) {
  const l = useLoad((s) => listSocialProviders(realm, s), `social:${realm}`);
  const [editing, setEditing] = useState<SocialProvider | "new">();
  const del = useRun(l.reload);
  const add = <Button tone="primary" onClick={() => setEditing("new")}>Add provider</Button>;
  return (
    <TableCard
      title="Social and OAuth providers"
      count={l.data?.length}
      description="Let users sign in with an account they already have. Client secrets are sealed on the server; providers from the server's oauth-providers file are read-only."
      actions={add}
    >
      <ActionError error={l.error} />
      {l.loading && !l.error && <SkeletonLines lines={3} label="Loading providers" />}
      {l.data && l.data.length === 0 && (
        <EmptyState compact
          title={enabled ? "No provider is registered for this realm" : "Social sign-in is off for this realm"}
          why={enabled ? "Add Google, GitHub, Apple or any OIDC provider so users can sign in with an account they already have." : "Turn on Social / OAuth under Realms > Settings, then add a provider."} />
      )}
      {l.data && l.data.length > 0 && (
        <ul aria-label="Social providers" className="divide-y divide-border">
          {l.data.map((p) => (
            <li key={p.id} className="flex flex-wrap items-center justify-between gap-3 px-4 py-3">
              <span className="flex min-w-0 items-center gap-3">
                <span aria-hidden="true" className="flex size-8 shrink-0 items-center justify-center rounded-lg bg-muted text-sm font-semibold uppercase text-muted-foreground">{(p.display_name ?? p.id).slice(0, 1)}</span>
                <span className="min-w-0">
                  <span className="block truncate font-medium">{p.display_name ?? p.id}</span>
                  <span className="flex flex-wrap items-center gap-1.5 text-xs text-muted-foreground">
                    <span className="font-mono">{p.id}</span>
                    <Tag>{p.kind}</Tag>
                    <span>{p.source === "file" ? "from file (read-only)" : "managed here"}</span>
                  </span>
                </span>
              </span>
              {!p.read_only && (
                <span className="flex gap-2">
                  <Button aria-label={`Edit provider ${p.id}`} onClick={() => setEditing(p)}>Edit</Button>
                  <ConfirmButton label={`Delete provider ${p.id}`} short="Delete" consequence="Users can no longer sign in with it. Their accounts stay." onConfirm={() => void del.run(() => deleteSocialProvider(realm, p.id))} />
                </span>
              )}
            </li>
          ))}
        </ul>
      )}
      <ActionError error={del.error} />
      <ProviderSheet key={editing === undefined ? "closed" : editing === "new" ? "new" : editing.id} open={editing !== undefined} onOpenChange={(v) => { if (!v) setEditing(undefined); }} realm={realm} existing={editing === "new" ? undefined : editing} onDone={l.reload} />
    </TableCard>
  );
}

function ProviderSheet({ open, onOpenChange, realm, existing, onDone }: { open: boolean; onOpenChange: (v: boolean) => void; realm: string; existing?: SocialProvider; onDone: () => void }) {
  const [id, setId] = useState(existing?.id ?? "");
  const [kind, setKind] = useState<ProviderKind>(existing?.kind ?? "google");
  const [clientId, setClientId] = useState(existing?.client_id ?? "");
  const [secret, setSecret] = useState<string | null>(existing?.client_secret ?? null);
  const [display, setDisplay] = useState(existing?.display_name ?? "");
  const [issuer, setIssuer] = useState(existing?.issuer ?? "");
  return (
    <CreateSheet
      open={open}
      onOpenChange={onOpenChange}
      title={existing ? `Edit provider ${existing.id}` : "New social provider"}
      description={`Realm ${realm}. Copy the client id and secret from the provider's developer console.`}
      submitLabel="Save provider"
      canSubmit={!!id && !!clientId}
      onSubmit={async () => {
        await putSocialProvider(realm, id, {
          kind, client_id: clientId, client_secret: secret, display_name: display || null, issuer: kind === "oidc" ? issuer || null : null,
          ...(existing?.apple ? { apple: existing.apple } : {}),
        });
        onDone();
        if (existing) { notify.saved(`Provider ${id}`); return; }
        return { what: `Provider ${id}` };
      }}
    >
      <div className="grid gap-3 sm:grid-cols-2">
        <Field label="Provider id" help={existing ? "Fixed once created." : "Used in the sign-in URL."}><Input size="sm" aria-label="Provider id" className="font-mono" value={id} disabled={!!existing} onChange={(e) => setId(e.target.value)} placeholder="google" /></Field>
        <Field label="Kind"><Sel aria-label="Provider kind" value={kind} onChange={(v) => setKind(v as ProviderKind)} options={PROVIDER_KINDS} /></Field>
        <Field label="Client id" className="sm:col-span-2"><Input size="sm" aria-label="Client id" className="font-mono" value={clientId} onChange={(e) => setClientId(e.target.value)} /></Field>
        <div className="sm:col-span-2"><SecretInput label="Client secret" stored={existing?.client_secret ?? null} onChange={setSecret} /></div>
        <Field label="Display name" help="The button text, such as Google."><Input size="sm" aria-label="Display name" value={display} onChange={(e) => setDisplay(e.target.value)} /></Field>
        {kind === "oidc" && <Field label="Issuer URL"><Input size="sm" aria-label="Issuer URL" className="font-mono" value={issuer} onChange={(e) => setIssuer(e.target.value)} placeholder="https://accounts.example.com" /></Field>}
      </div>
    </CreateSheet>
  );
}

function SmsGateway({ realm }: { realm: string }) {
  const l = useLoad((s) => getSmsGateway(realm, s), `sms:${realm}:gw`);
  const rm = useRun(l.reload);
  return (
    <TableCard
      title="SMS gateway"
      description="Who delivers the phone codes for this realm."
      actions={l.data?.configured ? <ConfirmButton label="Remove SMS gateway" short="Remove" consequence="Codes for this realm go through the instance gateway again." onConfirm={() => void rm.run(() => deleteSmsGateway(realm))} /> : undefined}
    >
      <ActionError error={l.error} />
      {l.loading && !l.error && <div className="p-4"><SkeletonLines lines={2} label="Loading SMS gateway" /></div>}
      {l.data && <GatewayForm key={`${realm}:${l.data.updated_at ?? "none"}`} realm={realm} current={l.data.configured ? l.data.config : null} onSaved={l.reload} />}
      <ActionError error={rm.error} />
    </TableCard>
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
    <form aria-label="SMS gateway" className="space-y-3 p-4" onSubmit={(e) => { e.preventDefault(); setSaved(false); void run(() => putSmsGateway(realm, cfg)); }}>
      <p className="text-xs text-muted-foreground">{current ? "This realm sends codes through its own gateway." : "No gateway set for this realm: the instance gateway is used."}</p>
      <Field label="Gateway type">
        <Sel aria-label="Gateway type" value={cfg.provider} onChange={(v) => setCfg({ provider: v as SmsProviderType })} options={[{ value: "dev", label: "Development (writes codes to disk)" }, { value: "twilio", label: "Twilio" }, { value: "webhook", label: "HTTPS webhook" }]} />
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
      <ActionError error={error} />
      <div className="flex items-center gap-3">
        <Button type="submit" tone="primary" disabled={busy}>{busy ? "Saving…" : "Save SMS gateway"}</Button>
        <Saved show={saved} />
      </div>
    </form>
  );
}

function Section({ title, description, badge, ident, reload, render }: { title: string; description: string; badge?: ReactNode; ident: RealmIdentity; reload: () => void; render: (cfg: RealmConfig, set: (c: RealmConfig) => void) => ReactNode }) {
  const [cfg, setCfg] = useState<RealmConfig>(ident.config);
  const [saved, setSaved] = useState(false);
  const { run, error, busy } = useRun(() => { setSaved(true); reload(); });
  return (
    <TableCard title={title} description={description} actions={badge}>
      <form aria-label={title} className="space-y-4 p-4" onSubmit={(e) => { e.preventDefault(); setSaved(false); void run(() => putRealmIdentity(ident.realm, { config: cfg })); }}>
        {render(cfg, setCfg)}
        <ActionError error={error} />
        <div className="flex items-center gap-3 border-t border-border pt-4">
          <Button type="submit" tone="primary" disabled={busy}>{busy ? "Saving…" : `Save ${title}`}</Button>
          <Saved show={saved} />
        </div>
      </form>
    </TableCard>
  );
}

/** A write-only secret input: shows whether a secret is stored; empty keeps it, typing replaces it, Clear removes it. */
function SecretInput({ label, stored, onChange }: { label: string; stored: string | null; onChange: (v: string | null) => void }) {
  const [v, setV] = useState("");
  const isSet = stored === SECRET_MASK;
  return (
    <Field label={label} help={isSet ? "A value is stored. Type to replace it, or clear it." : "Write-only: never shown again after saving."}>
      <span className="flex items-center gap-2">
        <Input size="sm" aria-label={label} type="password" autoComplete="off" value={v} placeholder={isSet ? "set (leave empty to keep)" : "not set"}
          onChange={(e) => { setV(e.target.value); onChange(e.target.value === "" ? (isSet ? SECRET_MASK : null) : e.target.value); }} />
        {isSet && <Button aria-label={`Clear ${label}`} onClick={() => { setV(""); onChange(""); }}>Clear</Button>}
      </span>
    </Field>
  );
}

function LdapForm({ cfg, set }: { cfg: RealmConfig; set: (c: RealmConfig) => void }) {
  const l: LdapSettings = cfg.ldap ?? DEFAULT_LDAP;
  const up = (p: Partial<LdapSettings>) => set({ ...cfg, ldap: { ...l, ...p } });
  const txt = (label: string, key: keyof LdapSettings, mono = false, placeholder?: string) => (
    <Field label={label}><Input size="sm" aria-label={label} className={`${mono ? "font-mono" : ""}`} value={(l[key] as string | null) ?? ""} placeholder={placeholder} onChange={(e) => up({ [key]: e.target.value } as Partial<LdapSettings>)} /></Field>
  );
  return (
    <>
      <Toggle checked={cfg.methods.ldap} onChange={(v) => set({ ...cfg, methods: { ...cfg.methods, ldap: v }, ldap: cfg.ldap ?? { ...DEFAULT_LDAP } })} label="LDAP sign-in enabled" />
      <div className="grid gap-3 sm:grid-cols-2">
        {txt("LDAP URL", "url", true, "ldaps://ldap.example.com")}{txt("Bind DN", "bind_dn", true)}
        <SecretInput label="Bind password" stored={l.bind_password} onChange={(v) => up({ bind_password: v })} />
        {txt("Base DN", "base_dn", true, "dc=example,dc=com")}
        {txt("User filter", "user_filter", true)}
        <Field label="Search scope">
          <Sel aria-label="Search scope" value={l.scope} onChange={(v) => up({ scope: v as LdapSettings["scope"] })} options={[{ value: "base", label: "Base only" }, { value: "one", label: "One level" }, { value: "sub", label: "Subtree" }]} />
        </Field>
        {txt("Email attribute", "email_attribute")}{txt("Name attribute", "name_attribute")}
      </div>
      <div className="grid gap-2 rounded-lg border border-border p-3 sm:grid-cols-2">
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
      <Toggle checked={cfg.methods.phone} onChange={(v) => set({ ...cfg, methods: { ...cfg.methods, phone: v } })} label="Phone sign-in enabled" />
      <Check label="A verified code may create an account" checked={cfg.phone.signup} onChange={(v) => set({ ...cfg, phone: { signup: v } })} />
      <Note>The gateway that delivers the codes is set in the SMS gateway panel.</Note>
    </>
  );
}

function TurnstileForm({ cfg, set }: { cfg: RealmConfig; set: (c: RealmConfig) => void }) {
  const t: TurnstileSettings = cfg.turnstile ?? DEFAULT_TURNSTILE;
  const up = (p: Partial<TurnstileSettings>) => set({ ...cfg, turnstile: { ...t, ...p } });
  return (
    <>
      <Toggle checked={t.enabled} onChange={(v) => up({ enabled: v })} label="Turnstile enabled" />
      <div className="grid gap-3 sm:grid-cols-3">
        <Field label="Site key"><Input size="sm" aria-label="Site key" className="font-mono" value={t.site_key} onChange={(e) => up({ site_key: e.target.value })} /></Field>
        <SecretInput label="Turnstile secret" stored={t.secret} onChange={(v) => up({ secret: v })} />
        <Field label="Allowed hostnames" help="Comma separated."><Input size="sm" aria-label="Allowed hostnames" value={t.hostnames.join(", ")} onChange={(e) => up({ hostnames: csv(e.target.value) })} placeholder="app.example.com" /></Field>
      </div>
      <MultiPick label="Protected actions" options={[...TURNSTILE_ACTIONS]} value={t.actions} onChange={(v) => up({ actions: v as TurnstileSettings["actions"] })} />
    </>
  );
}
