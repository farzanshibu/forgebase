import { useState } from "react";
import { EmptyState } from "../../components/EmptyState";
import { createRealmWithIdentity, DEFAULT_LDAP, listOidcKeys, putRealmIdentity, type LoginMethods, type RealmConfig, type RealmIdentity } from "../../api";
import { useAdminScope } from "../../auth/scope";
import { HelpTip } from "../../components/HelpTip";
import { Button } from "../../components/ui";
import { Sel, Toggle } from "../../components/controls";
import { CreateSheet, DetailSheet } from "../../components/CreateSheet";
import { FormField } from "../../components/FormField";
import { ActionError } from "../access/shared";
import { useRun } from "../access/util";
import { useLoad } from "../../hooks";
import { FormSection, TableCard, Tag } from "../billing/shared";
import { Check, Note, Saved } from "./shared";
import { defaultConfig, fmtIso } from "./util";
import { Table } from "panelui-web/table";
import { Input } from "panelui-web/input";
import { Badge } from "../../components/StatusBadge";
import { Th } from "../../components/TableCard";

const METHODS: { key: keyof LoginMethods; label: string }[] = [
  { key: "password", label: "Password" }, { key: "magic_link", label: "Magic link" }, { key: "email_otp", label: "Email code" },
  { key: "oauth", label: "Social / OAuth" }, { key: "passkey", label: "Passkey" }, { key: "phone", label: "Phone (SMS)" }, { key: "ldap", label: "LDAP" },
];

/** Realm ids are slugs (ADR-0008): lower-case letters, digits, dashes and underscores, at most 64. */
const SLUG = /^[a-z0-9_-]{1,64}$/;

/** Realm list, creation and tenant settings (sign-up, verified email, enabled sign-in methods). */
export default function RealmsPage({ realms, reload, current, onPick }: { realms: RealmIdentity[]; reload: () => void; current: string; onPick: (r: string) => void }) {
  const [editing, setEditing] = useState<string>();
  const [creating, setCreating] = useState(false);
  const [slug, setSlug] = useState("");
  const [name, setName] = useState("");
  const { platform } = useAdminScope();
  const editRealm = realms.find((r) => r.realm === editing);
  const slugError = slug && !SLUG.test(slug) ? "Lower-case letters, digits, dashes and underscores, at most 64." : undefined;
  const add = platform ? <Button tone="primary" onClick={() => setCreating(true)}>New realm</Button> : undefined;
  return (
    <div className="space-y-6">
      <TableCard title="Realms" count={realms.length} description="Each realm is a separate tenant with its own users, sign-in methods, apps and branding. The realm picked at the top is the one every tab edits." actions={add}>
        {realms.length === 0 && <EmptyState title="No realm has an identity configuration yet" why="A realm holds its own users and sign-in settings. Create one to set up sign-in for an app or a customer." action={add} />}
        {realms.length > 0 && (
          <Table framed aria-label="Realms">
            <Table.Header><Table.Row><Th>Realm</Th><Th>User collection</Th><Th>Sign-up</Th><Th>Sign-in methods</Th><Th>Updated</Th><Th><span className="sr-only">Actions</span></Th></Table.Row></Table.Header>
            <Table.Body>
              {realms.map((r) => (
                <Table.Row key={r.realm} aria-selected={r.realm === current || undefined}>
                  <Table.Cell className="font-mono font-medium">{r.realm}</Table.Cell>
                  <Table.Cell className="font-mono text-xs">{r.collection}</Table.Cell>
                  <Table.Cell><Badge status={r.config.signup ? "active" : "closed"}>{r.config.signup ? "open" : "closed"}</Badge></Table.Cell>
                  <Table.Cell>
                    <span className="flex max-w-md flex-wrap gap-1">
                      {METHODS.filter((m) => r.config.methods[m.key]).map((m) => <Tag key={m.key}>{m.label}</Tag>)}
                    </span>
                  </Table.Cell>
                  <Table.Cell className="whitespace-nowrap text-xs text-muted-foreground">{fmtIso(r.updated_at)}</Table.Cell>
                  <Table.Cell className="whitespace-nowrap text-right">
                    <span className="inline-flex gap-2">
                      <Button aria-label={`Use realm ${r.realm}`} disabled={r.realm === current} onClick={() => onPick(r.realm)}>{r.realm === current ? "Selected" : "Select"}</Button>
                      <Button aria-label={`Settings ${r.realm}`} onClick={() => setEditing(r.realm)}>Settings</Button>
                    </span>
                  </Table.Cell>
                </Table.Row>
              ))}
            </Table.Body>
          </Table>
        )}
      </TableCard>
      {platform && (
        <CreateSheet
          open={creating}
          onOpenChange={setCreating}
          title="New realm"
          description="Creates the realm, its user collection and a default identity configuration in one step."
          submitLabel="Create realm"
          canSubmit={!!slug && !slugError}
          onSubmit={async () => {
            await createRealmWithIdentity({ id: slug, name: name || slug, config: defaultConfig() });
            reload();
            onPick(slug);
            const what = `Realm ${slug}`;
            setSlug(""); setName("");
            return { what };
          }}
        >
          <FormField label="Realm id" help="Used in URLs and tokens; it cannot be changed later." error={slugError}>
            {(p) => <Input {...p} size="sm" className="font-mono" value={slug} onChange={(e) => setSlug(e.target.value)} placeholder="acme" />}
          </FormField>
          <FormField label="Realm name" help="A readable name. Defaults to the id.">
            {(p) => <Input {...p} size="sm" value={name} onChange={(e) => setName(e.target.value)} placeholder="Acme Inc." />}
          </FormField>
        </CreateSheet>
      )}
      <DetailSheet open={!!editRealm} onOpenChange={(v) => { if (!v) setEditing(undefined); }} title={<span className="font-mono">{editRealm?.realm}</span>} description="Tenant settings">
        {editRealm && <RealmSettings key={editRealm.realm} realm={editRealm} onSaved={reload} />}
      </DetailSheet>
    </div>
  );
}

function RealmSettings({ realm, onSaved }: { realm: RealmIdentity; onSaved: () => void }) {
  const [cfg, setCfg] = useState<RealmConfig>(realm.config);
  const [saved, setSaved] = useState(false);
  const { run, error, busy } = useRun(() => { setSaved(true); onSaved(); });
  const keys = useLoad((sig) => listOidcKeys(realm.realm, sig), `realm-keys:${realm.realm}`);
  return (
    <form aria-label="Tenant settings" className="space-y-5" onSubmit={(e) => { e.preventDefault(); setSaved(false); void run(() => putRealmIdentity(realm.realm, { config: cfg })); }}>
      <FormSection title="Sign-up">
        <Toggle checked={cfg.signup} onChange={(v) => setCfg({ ...cfg, signup: v })} label="Allow sign-up" description="New accounts via password, social, LDAP or phone." />
        <div className="flex flex-wrap items-center justify-between gap-3">
          <span className="text-sm">Require verified email</span>
          <Sel aria-label="Require verified email" value={cfg.require_verified_email === null ? "inherit" : String(cfg.require_verified_email)} onChange={(v) => setCfg({ ...cfg, require_verified_email: v === "inherit" ? null : v === "true" })} options={[{ value: "inherit", label: "Server default" }, { value: "true", label: "Yes" }, { value: "false", label: "No" }]} className="w-44" />
        </div>
      </FormSection>
      <FormSection title="Sign-in methods" description="What users of this realm may sign in with. Configure providers, LDAP and SMS under Identity providers.">
        <div className="grid grid-cols-2 gap-x-4 gap-y-2 rounded-lg border border-border p-3">
          {METHODS.map((m) => <Check key={m.key} label={m.label} checked={cfg.methods[m.key]} onChange={(v) => setCfg({ ...cfg, methods: { ...cfg.methods, [m.key]: v }, ldap: m.key === "ldap" && v && !cfg.ldap ? { ...DEFAULT_LDAP } : cfg.ldap })} />)}
        </div>
        <Check label="Phone code may create accounts" checked={cfg.phone.signup} onChange={(v) => setCfg({ ...cfg, phone: { signup: v } })} />
      </FormSection>
      <FormSection title="Details">
        <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-2 text-sm">
          <dt className="text-muted-foreground">User collection</dt>
          <dd><span className="font-mono">{realm.collection}</span> <span className="text-xs text-muted-foreground">(fixed once set)</span></dd>
          {keys.data && (
            <>
              <dt className="flex items-center gap-1 text-muted-foreground"><HelpTip term="oidc">OIDC</HelpTip> signing keys</dt>
              <dd data-testid="oidc-kids" className="flex flex-wrap gap-1">
                {keys.data.items.map((k) => <span key={k.kid} className="inline-flex items-center gap-1 font-mono text-xs">{k.kid} <Badge status={k.kid === keys.data!.current ? "current" : k.status} /></span>)}
              </dd>
            </>
          )}
        </dl>
        <Note>Rotate signing keys under Consents and keys.</Note>
      </FormSection>
      <ActionError error={error} />
      <div className="flex items-center gap-3 border-t border-border pt-4">
        <Button type="submit" tone="primary" disabled={busy}>{busy ? "Saving…" : "Save settings"}</Button>
        <Saved show={saved} />
      </div>
    </form>
  );
}
