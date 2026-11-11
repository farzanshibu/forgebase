import { Textarea } from "panelui-web/textarea";
import { EmptyState } from "../../components/EmptyState";
import { useState } from "react";
import { createClient, deleteClient, GRANT_TYPES, listClients, OIDC_SCOPES, rotateClientSecret, updateClient, type ClientApp, type ClientInput, type ClientType } from "../../api";
import { HelpTip } from "../../components/HelpTip";
import { Button, ConfirmButton, ErrorView, Loading, Panel } from "../../components/ui";
import { Sel } from "../../components/controls";
import { useLoad } from "../../hooks";
import { ActionError, Field, MultiPick } from "../access/shared";
import { useRun } from "../access/util";
import { Check, Note, SecretOnce } from "./shared";
import { fmtIso, lines, useRealm } from "./util";
import { Table } from "panelui-web/table";
import { Input } from "panelui-web/input";

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
        <p className="mb-2 flex items-center gap-1 text-sm text-muted-foreground">A client app is a program that signs users in through this realm with <HelpTip term="oidc">OIDC</HelpTip>.</p>
        <ErrorView error={l.error} />
        {l.loading && !l.error && <Loading what="Loading client apps" />}
        {l.data?.length === 0 && <EmptyState compact title="No client apps yet." why="A client app is a program that signs users in through this realm using OIDC. Create one below." />}
        {l.data && l.data.length > 0 && (
          <Table className="w-full text-left text-sm">
            <Table.Header className="text-xs uppercase tracking-wide text-muted-foreground"><Table.Row><Table.Head className="py-2">Name</Table.Head><Table.Head>Client id</Table.Head><Table.Head>Type</Table.Head><Table.Head>Secret</Table.Head><Table.Head /></Table.Row></Table.Header>
            <Table.Body>
              {l.data.map((c) => (
                <Table.Row key={c.client_id} className="border-t border-border">
                  <Table.Cell className="py-1.5">{c.name}{c.disabled && <span className="ml-2 rounded bg-destructive/15 px-1.5 text-xs text-destructive">disabled</span>}{c.skip_consent && <span className="ml-2 text-xs text-muted-foreground">first-party</span>}</Table.Cell>
                  <Table.Cell className="font-mono text-xs">{c.client_id}</Table.Cell>
                  <Table.Cell>{c.type}</Table.Cell>
                  <Table.Cell className="text-xs text-muted-foreground">
                    {c.type === "public" ? "none (PKCE)" : <>set {fmtIso(c.secret_created_at)}{c.previous_secret_expires_at && <span className="block text-warning">old secret valid until {fmtIso(c.previous_secret_expires_at)}</span>}</>}
                  </Table.Cell>
                  <Table.Cell className="space-x-2 whitespace-nowrap text-right">
                    <Button aria-label={`Edit ${c.name}`} onClick={() => setEditing(c.client_id)}>Edit</Button>
                    {c.type === "confidential" && (
                      <>
                        <Sel aria-label={`Grace for ${c.name}`} value={String(grace)} onChange={(v) => setGrace(Number(v))} options={GRACE.map((g) => ({ value: String(g.s), label: g.l }))} />
                        <ConfirmButton label={`Rotate secret ${c.name}`} onConfirm={() => void run(async () => {
                          const out = await rotateClientSecret(c.client_id, grace);
                          if (out.client_secret) setSecret({ label: `New secret for ${c.name}`, value: out.client_secret });
                        })} />
                      </>
                    )}
                    <ConfirmButton label={`Delete ${c.name}`} onConfirm={() => void run(() => deleteClient(c.client_id))} />
                  </Table.Cell>
                </Table.Row>
              ))}
            </Table.Body>
          </Table>
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
          <Field label="Name"><Input size="sm" aria-label="Name" required value={name} onChange={(e) => setName(e.target.value)} /></Field>
          <Field label="Description"><Input size="sm" aria-label="Description" value={description} onChange={(e) => setDescription(e.target.value)} /></Field>
          {!existing && (
            <Field label="Type">
              <Sel aria-label="Type" value={type} onChange={(v) => setType(v as ClientType)} options={[{ value: "confidential", label: "confidential (server, has a secret)" }, { value: "public", label: "public (browser or native, PKCE)" }]} />
            </Field>
          )}
        </div>
        <Field label="Redirect URIs (one per line, exact match)"><Textarea aria-label="Redirect URIs" minRows={3} className="font-mono" value={redirects} onChange={(e) => setRedirects(e.target.value)} /></Field>
        <Field label="Post-logout redirect URIs (one per line)"><Textarea aria-label="Post-logout redirect URIs" minRows={2} className="font-mono" value={logout} onChange={(e) => setLogout(e.target.value)} /></Field>
        <MultiPick label="Grant types" options={[...GRANT_TYPES]} value={grants} onChange={setGrants} />
        <MultiPick label="Scopes" options={[...OIDC_SCOPES]} value={scopes} onChange={setScopes} />
        <div className="flex flex-wrap items-end gap-3">
          <Field label="Access token lifetime (seconds, 60 to 86400)"><Input size="sm" aria-label="Access token lifetime" type="number" value={at} onChange={(e) => setAt(e.target.value)} /></Field>
          <Field label="Refresh token lifetime (seconds, 300 to 31536000)"><Input size="sm" aria-label="Refresh token lifetime" type="number" value={rt} onChange={(e) => setRt(e.target.value)} /></Field>
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
