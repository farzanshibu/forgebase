import { useState } from "react";
import { createClient, deleteClient, GRANT_TYPES, listClients, OIDC_SCOPES, rotateClientSecret, updateClient, type ClientApp, type ClientInput, type ClientType } from "../../api";
import { Button, ConfirmButton, ErrorView, Loading, Panel, inputCls } from "../../components/ui";
import { useLoad } from "../../hooks";
import { ActionError, Empty, Field, MultiPick } from "../access/shared";
import { useRun } from "../access/util";
import { Check, Note, SecretOnce } from "./shared";
import { fmtIso, lines, useRealm } from "./util";

const GRACE = [{ s: 0, l: "no grace (old secret stops now)" }, { s: 3600, l: "1 hour" }, { s: 86400, l: "1 day" }, { s: 604800, l: "7 days" }];

/** Client apps of the realm: create confidential or public apps, edit, rotate the secret with a grace period, delete. */
export default function ClientsPage() {
  const realm = useRealm();
  const l = useLoad((s) => listClients(realm, s), `clients:${realm}`);
  const [secret, setSecret] = useState<{ label: string; value: string }>();
  const [editing, setEditing] = useState<string>();
  const [grace, setGrace] = useState(3600);
  const { run, error } = useRun(l.reload);
  const edit = l.data?.find((c) => c.client_id === editing);
  return (
    <div className="space-y-6">
      {secret && <SecretOnce label={secret.label} secret={secret.value} onDismiss={() => setSecret(undefined)} />}
      <Panel title={`Client apps in ${realm}`}>
        <ErrorView error={l.error} />
        {l.loading && !l.error && <Loading what="Loading client apps" />}
        {l.data?.length === 0 && <Empty>No client apps yet.</Empty>}
        {l.data && l.data.length > 0 && (
          <table className="w-full text-left text-sm">
            <thead className="text-xs uppercase tracking-wide text-zinc-500"><tr><th className="py-2">Name</th><th>Client id</th><th>Type</th><th>Secret</th><th /></tr></thead>
            <tbody>
              {l.data.map((c) => (
                <tr key={c.client_id} className="border-t border-zinc-800">
                  <td className="py-1.5">{c.name}{c.disabled && <span className="ml-2 rounded bg-red-500/20 px-1.5 text-xs text-red-300">disabled</span>}{c.skip_consent && <span className="ml-2 text-xs text-zinc-500">first-party</span>}</td>
                  <td className="font-mono text-xs">{c.client_id}</td>
                  <td>{c.type}</td>
                  <td className="text-xs text-zinc-400">
                    {c.type === "public" ? "none (PKCE)" : <>set {fmtIso(c.secret_created_at)}{c.previous_secret_expires_at && <span className="block text-amber-300">old secret valid until {fmtIso(c.previous_secret_expires_at)}</span>}</>}
                  </td>
                  <td className="space-x-2 whitespace-nowrap text-right">
                    <Button aria-label={`Edit ${c.name}`} onClick={() => setEditing(c.client_id)}>Edit</Button>
                    {c.type === "confidential" && (
                      <>
                        <select aria-label={`Grace for ${c.name}`} className={inputCls} value={grace} onChange={(e) => setGrace(Number(e.target.value))}>
                          {GRACE.map((g) => <option key={g.s} value={g.s}>{g.l}</option>)}
                        </select>
                        <ConfirmButton label={`Rotate secret ${c.name}`} onConfirm={() => void run(async () => {
                          const out = await rotateClientSecret(c.client_id, grace);
                          if (out.client_secret) setSecret({ label: `New secret for ${c.name}`, value: out.client_secret });
                        })} />
                      </>
                    )}
                    <ConfirmButton label={`Delete ${c.name}`} onConfirm={() => void run(() => deleteClient(c.client_id))} />
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
        <ActionError error={error} />
      </Panel>
      {edit && <ClientForm key={edit.client_id} existing={edit} onDone={() => { setEditing(undefined); l.reload(); }} />}
      {!edit && <ClientForm realm={realm} onDone={l.reload} onSecret={(name, value) => setSecret({ label: `Secret for ${name}`, value })} />}
    </div>
  );
}

function ClientForm({ realm, existing, onDone, onSecret }: { realm?: string; existing?: ClientApp; onDone: () => void; onSecret?: (name: string, secret: string) => void }) {
  const [name, setName] = useState(existing?.name ?? "");
  const [description, setDescription] = useState(existing?.description ?? "");
  const [type, setType] = useState<ClientType>(existing?.type ?? "confidential");
  const [redirects, setRedirects] = useState((existing?.redirect_uris ?? []).join("\n"));
  const [logout, setLogout] = useState((existing?.post_logout_redirect_uris ?? []).join("\n"));
  const [grants, setGrants] = useState(existing?.grant_types ?? ["authorization_code", "refresh_token"]);
  const [scopes, setScopes] = useState(existing?.scopes ?? ["openid", "profile", "email", "offline_access"]);
  const [skip, setSkip] = useState(existing?.skip_consent ?? false);
  const [disabled, setDisabled] = useState(existing?.disabled ?? false);
  const [at, setAt] = useState(existing?.access_token_ttl ? String(existing.access_token_ttl) : "");
  const [rt, setRt] = useState(existing?.refresh_token_ttl ? String(existing.refresh_token_ttl) : "");
  const { run, error, busy } = useRun(onDone);
  const submit = () => run(async () => {
    const body = {
      name, description: description || undefined, redirect_uris: lines(redirects), post_logout_redirect_uris: lines(logout), grant_types: grants, scopes, skip_consent: skip,
      ...(at ? { access_token_ttl: Number(at) } : {}), ...(rt ? { refresh_token_ttl: Number(rt) } : {}),
    };
    if (existing) { await updateClient(existing.client_id, { ...body, disabled }); return; }
    const input: ClientInput = { ...body, realm, type };
    const out = await createClient(input);
    if (out.client_secret) onSecret?.(out.client.name, out.client_secret);
    setName(""); setRedirects("");
  });
  return (
    <Panel title={existing ? `Edit ${existing.name}` : "Create client app"}>
      <form aria-label={existing ? "Edit client app" : "Create client app"} className="space-y-3" onSubmit={(e) => { e.preventDefault(); void submit(); }}>
        <div className="flex flex-wrap gap-3">
          <Field label="Name"><input aria-label="Name" required className={inputCls} value={name} onChange={(e) => setName(e.target.value)} /></Field>
          <Field label="Description"><input aria-label="Description" className={inputCls} value={description} onChange={(e) => setDescription(e.target.value)} /></Field>
          {!existing && (
            <Field label="Type">
              <select aria-label="Type" className={inputCls} value={type} onChange={(e) => setType(e.target.value as ClientType)}>
                <option value="confidential">confidential (server, has a secret)</option><option value="public">public (browser or native, PKCE)</option>
              </select>
            </Field>
          )}
        </div>
        <Field label="Redirect URIs (one per line, exact match)"><textarea aria-label="Redirect URIs" rows={3} className={`${inputCls} font-mono`} value={redirects} onChange={(e) => setRedirects(e.target.value)} /></Field>
        <Field label="Post-logout redirect URIs (one per line)"><textarea aria-label="Post-logout redirect URIs" rows={2} className={`${inputCls} font-mono`} value={logout} onChange={(e) => setLogout(e.target.value)} /></Field>
        <MultiPick label="Grant types" options={[...GRANT_TYPES]} value={grants} onChange={setGrants} />
        <MultiPick label="Scopes" options={[...OIDC_SCOPES]} value={scopes} onChange={setScopes} />
        <div className="flex flex-wrap items-end gap-3">
          <Field label="Access token lifetime (seconds, 60 to 86400)"><input aria-label="Access token lifetime" type="number" className={inputCls} value={at} onChange={(e) => setAt(e.target.value)} /></Field>
          <Field label="Refresh token lifetime (seconds, 300 to 31536000)"><input aria-label="Refresh token lifetime" type="number" className={inputCls} value={rt} onChange={(e) => setRt(e.target.value)} /></Field>
        </div>
        <Check label="Skip consent screen" checked={skip} onChange={setSkip} hint="first-party apps only" />
        {existing && <Check label="Disabled" checked={disabled} onChange={setDisabled} />}
        <div className="flex items-center gap-3">
          <Button type="submit" tone="primary" disabled={busy || !name}>{existing ? "Save client app" : "Create client app"}</Button>
          {existing && <Button onClick={onDone}>Cancel</Button>}
        </div>
        {!existing && <Note>A confidential app's secret is shown once after creation.</Note>}
        <ActionError error={error} />
      </form>
    </Panel>
  );
}
