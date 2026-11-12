import { Textarea } from "panelui-web/textarea";
import { EmptyState } from "../../components/EmptyState";
import { useState } from "react";
import { createClient, deleteClient, GRANT_TYPES, listClients, OIDC_SCOPES, rotateClientSecret, updateClient, type ClientApp, type ClientInput, type ClientType } from "../../api";
import { HelpTip } from "../../components/HelpTip";
import { Button, ConfirmButton } from "../../components/ui";
import { Sel } from "../../components/controls";
import { ConfirmDialog } from "../../components/ConfirmDialog";
import { CreateSheet } from "../../components/CreateSheet";
import { FormField } from "../../components/FormField";
import { SkeletonTable } from "../../components/Skeleton";
import { useLoad } from "../../hooks";
import { notify } from "../../toaster";
import { ActionError } from "../access/shared";
import { useRun } from "../access/util";
import { FormSection, TableCard, Tag } from "../billing/shared";
import { Check, MultiPick, SecretOnce } from "./shared";
import { fmtIso, lines, useRealm } from "./util";
import { Table } from "panelui-web/table";
import { Input } from "panelui-web/input";
import { Badge } from "../../components/StatusBadge";
import { IdText } from "../../components/IdText";
import { Th } from "../../components/TableCard";

const GRACE = [{ s: 0, l: "No grace: the old secret stops now" }, { s: 3600, l: "1 hour" }, { s: 86400, l: "1 day" }, { s: 604800, l: "7 days" }];

/** Client apps of the realm: create confidential or public apps, edit, rotate the secret with a grace period, delete. */
export default function ClientsPage() {
  const realm = useRealm();
  const l = useLoad((s) => listClients(realm, s), `clients:${realm}`);
  const [secret, setSecret] = useState<{ label: string; value: string }>();
  const [editing, setEditing] = useState<string>();
  const [creating, setCreating] = useState(false);
  const [grace, setGrace] = useState(3600);
  const { run, error } = useRun(l.reload);
  const edit = l.data?.find((c) => c.client_id === editing);
  const add = <Button tone="primary" onClick={() => setCreating(true)}>New client app</Button>;
  return (
    <div className="space-y-6">
      {secret && <SecretOnce label={secret.label} secret={secret.value} onDismiss={() => setSecret(undefined)} />}
      <TableCard
        title="Client apps"
        count={l.data?.length}
        description={<span className="inline-flex flex-wrap items-center gap-1">Programs that sign users in through realm <span className="font-mono">{realm}</span> using <HelpTip term="oidc">OIDC</HelpTip></span>}
        actions={add}
      >
        <ActionError error={l.error} />
        {l.loading && !l.error && <SkeletonTable rows={3} columns={4} label="Loading client apps" />}
        {l.data?.length === 0 && (
          <EmptyState title="No client apps yet" why="A client app is a website, mobile app or server that signs users in through this realm using OIDC. Create one to get a client id." action={add} />
        )}
        {l.data && l.data.length > 0 && (
          <Table framed aria-label="Client apps">
            <Table.Header><Table.Row><Th>App</Th><Th>Client id</Th><Th>Type</Th><Th>Secret</Th><Th><span className="sr-only">Actions</span></Th></Table.Row></Table.Header>
            <Table.Body>
              {l.data.map((c) => (
                <Table.Row key={c.client_id}>
                  <Table.Cell>
                    <span className="flex flex-wrap items-center gap-1.5">
                      <span className="font-medium">{c.name}</span>
                      {c.disabled && <Badge status="disabled" />}
                      {c.skip_consent && <Tag>first-party</Tag>}
                    </span>
                    {c.description && <span className="block max-w-xs truncate text-xs text-muted-foreground" title={c.description}>{c.description}</span>}
                  </Table.Cell>
                  <Table.Cell><IdText value={c.client_id} what="client id" /></Table.Cell>
                  <Table.Cell><Tag>{c.type}</Tag></Table.Cell>
                  <Table.Cell className="text-xs text-muted-foreground">
                    {c.type === "public" ? "none (PKCE)" : <>set {fmtIso(c.secret_created_at)}{c.previous_secret_expires_at && <span className="block text-warning">old secret valid until {fmtIso(c.previous_secret_expires_at)}</span>}</>}
                  </Table.Cell>
                  <Table.Cell className="whitespace-nowrap text-right">
                    <span className="inline-flex gap-2">
                      <Button aria-label={`Edit ${c.name}`} onClick={() => setEditing(c.client_id)}>Edit</Button>
                      {c.type === "confidential" && (
                        <ConfirmDialog
                          trigger={<Button aria-label={`Rotate secret ${c.name}`} />}
                          triggerLabel="Rotate secret"
                          tone="warning"
                          title={`Rotate the secret of ${c.name}?`}
                          consequence="A new secret is made and shown once. Choose how long the old one keeps working, so you can deploy the new one first."
                          confirmLabel="Rotate secret"
                          onConfirm={() => void run(async () => {
                            const out = await rotateClientSecret(c.client_id, grace);
                            if (out.client_secret) setSecret({ label: `New secret for ${c.name}`, value: out.client_secret });
                          })}
                        >
                          <Sel aria-label={`Grace for ${c.name}`} value={String(grace)} onChange={(v) => setGrace(Number(v))} options={GRACE.map((g) => ({ value: String(g.s), label: g.l }))} className="w-full" />
                        </ConfirmDialog>
                      )}
                      <ConfirmButton label={`Delete ${c.name}`} short="Delete" consequence="Sign-ins through this app stop at once and its tokens can no longer be refreshed." onConfirm={() => void run(() => deleteClient(c.client_id))} />
                    </span>
                  </Table.Cell>
                </Table.Row>
              ))}
            </Table.Body>
          </Table>
        )}
      </TableCard>
      <ActionError error={error} />
      <ClientSheet key={edit?.client_id ?? `new:${realm}`} open={creating || !!edit} onOpenChange={(v) => { if (!v) { setCreating(false); setEditing(undefined); } }} realm={realm} existing={edit}
        onDone={l.reload} onSecret={(name, value) => setSecret({ label: `Secret for ${name}`, value })} />
    </div>
  );
}

function ClientSheet({ open, onOpenChange, realm, existing, onDone, onSecret }: { open: boolean; onOpenChange: (v: boolean) => void; realm: string; existing?: ClientApp; onDone: () => void; onSecret: (name: string, secret: string) => void }) {
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
  const badUri = lines(redirects).find((u) => !/^[a-z][a-z0-9+.-]*:\/\/./i.test(u));
  const ttl = (v: string, min: number, max: number) => (v === "" || (/^\d+$/.test(v) && Number(v) >= min && Number(v) <= max) ? undefined : `Seconds from ${min} to ${max}`);
  const atError = ttl(at, 60, 86400);
  const rtError = ttl(rt, 300, 31536000);
  return (
    <CreateSheet
      open={open}
      onOpenChange={onOpenChange}
      title={existing ? "Edit client app" : "New client app"}
      description={existing ? <span>{existing.name} · <span className="font-mono">{existing.client_id}</span></span> : `A program that signs users in through realm ${realm}.`}
      submitLabel={existing ? "Save client app" : "Create client app"}
      canSubmit={!!name.trim() && !badUri && !atError && !rtError}
      onSubmit={async () => {
        const body = {
          name, description: description || undefined, redirect_uris: lines(redirects), post_logout_redirect_uris: lines(logout), grant_types: grants, scopes, skip_consent: skip,
          ...(at ? { access_token_ttl: Number(at) } : {}), ...(rt ? { refresh_token_ttl: Number(rt) } : {}),
        };
        if (existing) {
          await updateClient(existing.client_id, { ...body, disabled });
          onDone();
          notify.saved(`Client app ${name}`);
          return;
        }
        const input: ClientInput = { ...body, realm, type };
        const out = await createClient(input);
        onDone();
        if (out.client_secret) onSecret(out.client.name, out.client_secret);
        setName(""); setDescription(""); setRedirects(""); setLogout("");
        return { what: `Client app ${out.client.name}` };
      }}
    >
      <FormSection title="Basics">
        <FormField label="Name" help="Shown to users on the consent screen.">
          {(p) => <Input {...p} size="sm" value={name} onChange={(e) => setName(e.target.value)} placeholder="Web app" />}
        </FormField>
        <FormField label="Description">
          {(p) => <Input {...p} size="sm" value={description} onChange={(e) => setDescription(e.target.value)} />}
        </FormField>
        {!existing && (
          <FormField label="Type" help="Confidential apps run on a server and keep a secret. Public apps run in a browser or on a device and use PKCE instead.">
            {(p) => <Sel {...p} value={type} onChange={(v) => setType(v as ClientType)} options={[{ value: "confidential", label: "Confidential (server, has a secret)" }, { value: "public", label: "Public (browser or native, PKCE)" }]} />}
          </FormField>
        )}
      </FormSection>
      <FormSection title="Redirects" description="Exact URLs, one per line. Users are only ever sent back to these.">
        <FormField label="Redirect URIs" error={badUri ? `${badUri} is not a full URL` : undefined}>
          {(p) => <Textarea {...p} minRows={3} className="font-mono text-xs" value={redirects} onChange={(e) => setRedirects(e.target.value)} placeholder="https://app.example.com/callback" />}
        </FormField>
        <FormField label="Post-logout redirect URIs">
          {(p) => <Textarea {...p} minRows={2} className="font-mono text-xs" value={logout} onChange={(e) => setLogout(e.target.value)} placeholder="https://app.example.com/" />}
        </FormField>
      </FormSection>
      <FormSection title="Grants and scopes">
        <MultiPick label="Grant types" options={[...GRANT_TYPES]} value={grants} onChange={setGrants} />
        <MultiPick label="Scopes" options={[...OIDC_SCOPES]} value={scopes} onChange={setScopes} />
      </FormSection>
      <FormSection title="Token lifetimes" description="Blank uses the realm default.">
        <div className="grid gap-3 sm:grid-cols-2">
          <FormField label="Access token lifetime" help="Seconds, 60 to 86400." error={atError}>
            {(p) => <Input {...p} size="sm" className="tabular-nums" inputMode="numeric" value={at} onChange={(e) => setAt(e.target.value)} placeholder="3600" />}
          </FormField>
          <FormField label="Refresh token lifetime" help="Seconds, 300 to 31536000." error={rtError}>
            {(p) => <Input {...p} size="sm" className="tabular-nums" inputMode="numeric" value={rt} onChange={(e) => setRt(e.target.value)} placeholder="2592000" />}
          </FormField>
        </div>
      </FormSection>
      <FormSection title="Options">
        <Check label="Skip consent screen" checked={skip} onChange={setSkip} hint="First-party apps only: users are not asked to allow access." />
        {existing && <Check label="Disabled" checked={disabled} onChange={setDisabled} hint="Sign-ins through this app are refused until it is enabled again." />}
        {!existing && type === "confidential" && <p className="text-xs text-muted-foreground">The app's secret is shown once after it is created.</p>}
      </FormSection>
    </CreateSheet>
  );
}
