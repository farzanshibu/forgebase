import { useState } from "react";
import { Input } from "panelui-web/input";
import { Table } from "panelui-web/table";
import {
  createServiceAccount, deleteServiceAccount, listServiceAccounts, revokeKey, serviceAccountKeys, type ApiKeyView, type ServiceAccountView,
} from "../../api";
import { Sel } from "../../components/controls";
import { ConfirmDialog } from "../../components/ConfirmDialog";
import { CreateSheet } from "../../components/CreateSheet";
import { EmptyState } from "../../components/EmptyState";
import { ErrorPanel } from "../../components/ErrorPanel";
import { FormField } from "../../components/FormField";
import { SkeletonTable } from "../../components/Skeleton";
import { Button, Panel } from "../../components/ui";
import { fieldErrors } from "../../errors";
import { useCreateParam, useLoad } from "../../hooks";
import { notify } from "../../toaster";
import { validate } from "../../validate";
import CreateKeyDialog from "./CreateKeyDialog";
import { Badge, THEAD, Th, fmtTime, useAction } from "./common";

/** Service accounts: the identities that own API keys. Most developers never come here: "Create API key" makes one. */
export default function ServiceAccounts() {
  const list = useLoad(listServiceAccounts, "sas");
  const [creating, setCreating] = useCreateParam();
  const [open, setOpen] = useState<string>();
  const current = list.data?.find((s) => s.id === open);
  return (
    <div className="space-y-4">
      <ErrorPanel error={list.error} onRetry={list.reload} />
      {list.loading && !list.error && <SkeletonTable rows={3} columns={5} label="Loading service accounts" />}
      {list.data && (
        <div className="flex flex-wrap items-center justify-between gap-2">
          <p className="max-w-2xl text-sm text-muted-foreground">
            A service account is an identity for a program, not a person. Each API key belongs to one: turn the account off or delete it and its keys stop working.
          </p>
          <Button tone="primary" onClick={() => setCreating(true)}>New service account</Button>
        </div>
      )}
      {list.data?.length === 0 && (
        <EmptyState
          title="No service accounts yet"
          why="A service account owns the API keys your apps use. Creating an API key makes one for you, or make one here first."
          action={<Button tone="primary" onClick={() => setCreating(true)}>New service account</Button>}
          docs="docs/adr/0004-auth.md"
        />
      )}
      {!!list.data?.length && (
        <Table className="w-full text-sm" aria-label="Service accounts">
          <Table.Header className={THEAD}>
            <Table.Row><Th>Name</Th><Th>Role</Th><Th>Created</Th><Th>Status</Th><Th> </Th></Table.Row>
          </Table.Header>
          <Table.Body>
            {list.data.map((s) => (
              <Table.Row key={s.id} className="border-t border-border">
                <Table.Cell className="py-2 pr-3">
                  {s.name} <span className="text-xs text-muted-foreground">{s.description}</span>
                </Table.Cell>
                <Table.Cell className="pr-3">{s.role === "admin" ? <Badge tone="warn">admin</Badge> : s.role}</Table.Cell>
                <Table.Cell className="pr-3">{fmtTime(s.created_at)}</Table.Cell>
                <Table.Cell className="pr-3">{s.disabled ? <Badge tone="bad">disabled</Badge> : <Badge tone="ok">active</Badge>}</Table.Cell>
                <Table.Cell className="space-x-2 whitespace-nowrap">
                  <Button aria-label={`Keys of ${s.name}`} onClick={() => setOpen(open === s.id ? undefined : s.id)}>Keys</Button>
                  <ConfirmDialog
                    trigger={<Button tone="danger" aria-label={`Delete ${s.name}`}>Delete</Button>}
                    title={`Delete service account ${s.name}?`}
                    consequence="Its keys stop working at once. Apps that use them get 401."
                    confirmLabel="Delete service account"
                    confirmText={s.name}
                    onConfirm={async () => {
                      await deleteServiceAccount(s.id);
                      notify.deleted(`Service account ${s.name}`);
                      if (open === s.id) setOpen(undefined);
                      list.reload();
                    }}
                  />
                </Table.Cell>
              </Table.Row>
            ))}
          </Table.Body>
        </Table>
      )}
      {current && <Keys sa={current} />}
      <NewAccountSheet open={creating} onOpenChange={setCreating} onCreated={list.reload} />
    </div>
  );
}

function NewAccountSheet({ open, onOpenChange, onCreated }: { open: boolean; onOpenChange: (o: boolean) => void; onCreated: () => void }) {
  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  const [role, setRole] = useState("service");
  const [touched, setTouched] = useState(false);
  const [saveError, setSaveError] = useState<unknown>();
  const nameError = (touched ? validate.required("a name")(name) : undefined) ?? fieldErrors(saveError).name;
  return (
    <CreateSheet
      open={open} onOpenChange={onOpenChange} title="New service account" description="An identity for a program. Give it keys afterwards."
      submitLabel="Create service account" canSubmit={name.trim() !== ""} onError={setSaveError}
      onSubmit={async () => {
        await createServiceAccount({ name: name.trim(), description, role });
        onCreated();
        const what = `Service account ${name.trim()}`;
        setName(""); setDescription(""); setTouched(false);
        return { what };
      }}
    >
      <FormField label="Name" required error={nameError} help="Name it after the app or service that will use it.">
        {(p) => <Input {...p} className="w-full" value={name} onChange={(e) => { setName(e.target.value); setTouched(true); }} />}
      </FormField>
      <FormField label="Description" help="Optional. Say what it is for.">
        {(p) => <Input {...p} className="w-full" value={description} onChange={(e) => setDescription(e.target.value)} />}
      </FormField>
      <FormField label="Role" term="role" help="Service can use the API within its keys' scopes. Admin can also manage the server: give it only to a trusted tool.">
        {(p) => <Sel {...p} size="md" className="w-full" value={role} onChange={setRole} options={["service", "admin"]} />}
      </FormField>
    </CreateSheet>
  );
}

function Keys({ sa }: { sa: ServiceAccountView }) {
  const keys = useLoad((s) => serviceAccountKeys(sa.id, s), `sak:${sa.id}`);
  const [creating, setCreating] = useState(false);
  return (
    <Panel title={`Keys of ${sa.name}`} actions={<Button tone="primary" onClick={() => setCreating(true)}>Create key</Button>}>
      <ErrorPanel error={keys.error} onRetry={keys.reload} />
      {keys.loading && !keys.error && <SkeletonTable rows={2} columns={5} label="Loading keys" />}
      {keys.data?.length === 0 && <EmptyState compact title="No keys yet" why="Create a key for this account so an app can sign in as it." />}
      {!!keys.data?.length && <KeyTable rows={keys.data} onRevoked={keys.reload} />}
      <CreateKeyDialog open={creating} onOpenChange={setCreating} accountId={sa.id} onCreated={keys.reload} />
    </Panel>
  );
}

export function KeyTable({ rows, onRevoked, showOwner }: { rows: ApiKeyView[]; onRevoked: () => void; showOwner?: boolean }) {
  const act = useAction();
  return (
    <>
      {act.errorView}
      <Table className="w-full text-sm" aria-label="Keys">
        <Table.Header className={THEAD}>
          <Table.Row><Th>Name</Th>{showOwner && <Th>Owner</Th>}<Th>Scopes</Th><Th>Tenant</Th><Th>Expires</Th><Th>Last used</Th><Th> </Th></Table.Row>
        </Table.Header>
        <Table.Body>
          {rows.map((k) => (
            <Table.Row key={k.id} className="border-t border-border">
              <Table.Cell className="py-2 pr-3">
                {k.name} <span className="font-mono text-xs text-muted-foreground">{k.id}</span>
              </Table.Cell>
              {showOwner && <Table.Cell className="pr-3 font-mono text-xs">{k.owner_type}:{k.owner_id}</Table.Cell>}
              <Table.Cell className="pr-3 text-xs">{k.scopes.map((s) => <code key={s} className="me-1 inline-block">{s}</code>)}</Table.Cell>
              <Table.Cell className="pr-3 font-mono text-xs">{k.tenant ?? <span className="text-muted-foreground">any</span>}</Table.Cell>
              <Table.Cell className="pr-3">{fmtTime(k.expires_at)}</Table.Cell>
              <Table.Cell className="pr-3">{fmtTime(k.last_used_at)}</Table.Cell>
              <Table.Cell>
                {k.revoked ? (
                  <Badge tone="bad">revoked</Badge>
                ) : (
                  <ConfirmDialog
                    trigger={<Button tone="danger" aria-label={`Revoke ${k.name}`}>Revoke</Button>}
                    title={`Revoke ${k.name}?`}
                    consequence="Anything that uses this key gets 401 from now on. This cannot be undone."
                    confirmLabel="Revoke key"
                    onConfirm={async () => {
                      await revokeKey(k.id);
                      notify.deleted(`Key ${k.name}`);
                      onRevoked();
                    }}
                  />
                )}
              </Table.Cell>
            </Table.Row>
          ))}
        </Table.Body>
      </Table>
    </>
  );
}
