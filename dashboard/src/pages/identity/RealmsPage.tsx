import { useState } from "react";
import { createRealm, DEFAULT_LDAP, putRealmIdentity, type LoginMethods, type RealmConfig, type RealmIdentity } from "../../api";
import { Button, Panel, inputCls } from "../../components/ui";
import { ActionError, Empty, TextField } from "../access/shared";
import { useRun } from "../access/util";
import { Check, Note } from "./shared";
import { defaultConfig, fmtIso } from "./util";

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
          <table className="w-full text-left text-sm">
            <thead className="text-xs uppercase tracking-wide text-zinc-500"><tr><th className="py-2">Realm</th><th>User collection</th><th>Sign-up</th><th>Methods</th><th>Updated</th><th /></tr></thead>
            <tbody>
              {realms.map((r) => (
                <tr key={r.realm} className="border-t border-zinc-800">
                  <td className="py-1.5 font-mono">{r.realm}</td>
                  <td className="font-mono text-xs">{r.collection}</td>
                  <td>{r.config.signup ? "open" : "closed"}</td>
                  <td className="text-xs text-zinc-400">{METHODS.filter((m) => r.config.methods[m.key]).map((m) => m.label).join(", ")}</td>
                  <td className="text-xs text-zinc-500">{fmtIso(r.updated_at)}</td>
                  <td className="space-x-2 text-right">
                    <Button aria-label={`Use realm ${r.realm}`} onClick={() => onPick(r.realm)}>Select</Button>
                    <Button aria-label={`Settings ${r.realm}`} onClick={() => setEditing(r.realm)}>Settings</Button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </Panel>
      <Panel title="Create realm">
        <div className="flex flex-wrap items-end gap-3">
          <TextField label="Realm id" value={slug} onChange={setSlug} placeholder="acme" />
          <TextField label="Realm name" value={name} onChange={setName} />
          <Button tone="primary" disabled={!slug || create.busy}
            onClick={() => void create.run(async () => {
              await createRealm(slug, name || slug);
              await putRealmIdentity(slug, { config: defaultConfig() });
              onPick(slug);
              setSlug(""); setName("");
            })}>Create realm</Button>
        </div>
        <Note>Creates the realm, then its identity configuration with the default settings; its user collection is created on first save.</Note>
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
  return (
    <Panel title={`Tenant settings: ${realm.realm}`}>
      <form aria-label="Tenant settings" className="space-y-3" onSubmit={(e) => { e.preventDefault(); setSaved(false); void run(() => putRealmIdentity(realm.realm, { config: cfg })); }}>
        <p className="text-xs text-zinc-500">User collection: <span className="font-mono">{realm.collection}</span> (fixed once set)</p>
        <Check label="Allow sign-up" checked={cfg.signup} onChange={(v) => setCfg({ ...cfg, signup: v })} hint="new accounts via password, social, LDAP or phone" />
        <label className="flex items-center gap-2 text-sm text-zinc-300">
          Require verified email
          <select aria-label="Require verified email" className={inputCls}
            value={cfg.require_verified_email === null ? "inherit" : String(cfg.require_verified_email)}
            onChange={(e) => setCfg({ ...cfg, require_verified_email: e.target.value === "inherit" ? null : e.target.value === "true" })}>
            <option value="inherit">server default</option><option value="true">yes</option><option value="false">no</option>
          </select>
        </label>
        <fieldset className="space-y-1"><legend className="text-xs text-zinc-400">Sign-in methods</legend>
          <div className="flex flex-wrap gap-x-4 gap-y-1">
            {METHODS.map((m) => <Check key={m.key} label={m.label} checked={cfg.methods[m.key]} onChange={(v) => setCfg({ ...cfg, methods: { ...cfg.methods, [m.key]: v }, ldap: m.key === "ldap" && v && !cfg.ldap ? { ...DEFAULT_LDAP } : cfg.ldap })} />)}
          </div>
        </fieldset>
        <Check label="Phone code may create accounts" checked={cfg.phone.signup} onChange={(v) => setCfg({ ...cfg, phone: { signup: v } })} />
        <div className="flex items-center gap-3">
          <Button type="submit" tone="primary" disabled={busy}>Save settings</Button>
          {saved && <span role="status" className="text-sm text-emerald-300">Saved</span>}
        </div>
        <ActionError error={error} />
      </form>
    </Panel>
  );
}
