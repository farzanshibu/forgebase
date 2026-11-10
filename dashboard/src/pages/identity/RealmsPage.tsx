import { useState } from "react";
import { createRealmWithIdentity, DEFAULT_LDAP, listOidcKeys, putRealmIdentity, type LoginMethods, type RealmConfig, type RealmIdentity } from "../../api";
import { Button, Panel } from "../../components/ui";
import { Sel } from "../../components/controls";
import { ActionError, Empty, TextField } from "../access/shared";
import { useRun } from "../access/util";
import { useLoad } from "../../hooks";
import { Check, Note } from "./shared";
import { defaultConfig, fmtIso } from "./util";
import { Table } from "panelui-web/table";

const METHODS: { key: keyof LoginMethods; label: string }[] = [
  { key: "password", label: "Password" }, { key: "magic_link", label: "Magic link" }, { key: "email_otp", label: "Email code" },
  { key: "oauth", label: "Social / OAuth" }, { key: "passkey", label: "Passkey" }, { key: "phone", label: "Phone (SMS)" }, { key: "ldap", label: "LDAP" },
];

/** Realm list, creation and tenant settings (sign-up, verified email, enabled sign-in methods). */
export default function RealmsPage({ realms, reload, onPick }: { realms: RealmIdentity[]; reload: () => void; onPick: (r: string) => void }) {
  const [editing, setEditing] = useState<string>();
  const [slug, setSlug] = useState("");
  const [name, setName] = useState("");
  const create = useRun(reload);
  const current = realms.find((r) => r.realm === editing);
  return (
    <div className="space-y-6">
      <Panel title="Realms">
        {realms.length === 0 && <Empty>No realm has an identity configuration yet.</Empty>}
        {realms.length > 0 && (
          <Table className="w-full text-left text-sm">
            <Table.Header className="text-xs uppercase tracking-wide text-muted-foreground"><Table.Row><Table.Head className="py-2">Realm</Table.Head><Table.Head>User collection</Table.Head><Table.Head>Sign-up</Table.Head><Table.Head>Methods</Table.Head><Table.Head>Updated</Table.Head><Table.Head /></Table.Row></Table.Header>
            <Table.Body>
              {realms.map((r) => (
                <Table.Row key={r.realm} className="border-t border-border">
                  <Table.Cell className="py-1.5 font-mono">{r.realm}</Table.Cell>
                  <Table.Cell className="font-mono text-xs">{r.collection}</Table.Cell>
                  <Table.Cell>{r.config.signup ? "open" : "closed"}</Table.Cell>
                  <Table.Cell className="text-xs text-muted-foreground">{METHODS.filter((m) => r.config.methods[m.key]).map((m) => m.label).join(", ")}</Table.Cell>
                  <Table.Cell className="text-xs text-muted-foreground">{fmtIso(r.updated_at)}</Table.Cell>
                  <Table.Cell className="space-x-2 text-right">
                    <Button aria-label={`Use realm ${r.realm}`} onClick={() => onPick(r.realm)}>Select</Button>
                    <Button aria-label={`Settings ${r.realm}`} onClick={() => setEditing(r.realm)}>Settings</Button>
                  </Table.Cell>
                </Table.Row>
              ))}
            </Table.Body>
          </Table>
        )}
      </Panel>
      <Panel title="Create realm">
        <div className="flex flex-wrap items-end gap-3">
          <TextField label="Realm id" value={slug} onChange={setSlug} placeholder="acme" />
          <TextField label="Realm name" value={name} onChange={setName} />
          <Button tone="primary" disabled={!slug || create.busy}
            onClick={() => void create.run(async () => {
              await createRealmWithIdentity({ id: slug, name: name || slug, config: defaultConfig() });
              onPick(slug);
              setSlug(""); setName("");
            })}>Create realm</Button>
        </div>
        <Note>Creates the realm and its identity configuration (default settings) in one call; its user collection is created with it.</Note>
        <ActionError error={create.error} />
      </Panel>
      {current && <RealmSettings key={current.realm} realm={current} onSaved={reload} />}
    </div>
  );
}

function RealmSettings({ realm, onSaved }: { realm: RealmIdentity; onSaved: () => void }) {
  const [cfg, setCfg] = useState<RealmConfig>(realm.config);
  const [saved, setSaved] = useState(false);
  const { run, error, busy } = useRun(() => { setSaved(true); onSaved(); });
  const keys = useLoad((sig) => listOidcKeys(realm.realm, sig), `realm-keys:${realm.realm}`);
  return (
    <Panel title={`Tenant settings: ${realm.realm}`}>
      <form aria-label="Tenant settings" className="space-y-3" onSubmit={(e) => { e.preventDefault(); setSaved(false); void run(() => putRealmIdentity(realm.realm, { config: cfg })); }}>
        <p className="text-xs text-muted-foreground">User collection: <span className="font-mono">{realm.collection}</span> (fixed once set)</p>
        {keys.data && (
          <p className="text-xs text-muted-foreground" data-testid="oidc-kids">
            OIDC signing keys: {keys.data.items.map((k) => `${k.kid} (${k.kid === keys.data!.current ? "current" : k.status})`).join(", ")}
          </p>
        )}
        <Check label="Allow sign-up" checked={cfg.signup} onChange={(v) => setCfg({ ...cfg, signup: v })} hint="new accounts via password, social, LDAP or phone" />
        <label className="flex items-center gap-2 text-sm text-foreground/80">
          Require verified email
          <Sel aria-label="Require verified email" value={cfg.require_verified_email === null ? "inherit" : String(cfg.require_verified_email)} onChange={(v) => setCfg({ ...cfg, require_verified_email: v === "inherit" ? null : v === "true" })} options={[{ value: "inherit", label: "server default" }, { value: "true", label: "yes" }, { value: "false", label: "no" }]} />
        </label>
        <fieldset className="space-y-1"><legend className="text-xs text-muted-foreground">Sign-in methods</legend>
          <div className="flex flex-wrap gap-x-4 gap-y-1">
            {METHODS.map((m) => <Check key={m.key} label={m.label} checked={cfg.methods[m.key]} onChange={(v) => setCfg({ ...cfg, methods: { ...cfg.methods, [m.key]: v }, ldap: m.key === "ldap" && v && !cfg.ldap ? { ...DEFAULT_LDAP } : cfg.ldap })} />)}
          </div>
        </fieldset>
        <Check label="Phone code may create accounts" checked={cfg.phone.signup} onChange={(v) => setCfg({ ...cfg, phone: { signup: v } })} />
        <div className="flex items-center gap-3">
          <Button type="submit" tone="primary" disabled={busy}>Save settings</Button>
          {saved && <span role="status" className="text-sm text-success">Saved</span>}
        </div>
        <ActionError error={error} />
      </form>
    </Panel>
  );
}
