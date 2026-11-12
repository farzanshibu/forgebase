import { Robot01Icon } from "@hugeicons/core-free-icons";
import { HugeiconsIcon } from "@hugeicons/react";
import { useMemo, useState } from "react";
import { Input } from "panelui-web/input";
import { Table } from "panelui-web/table";
import {
  createServiceAccount, deleteServiceAccount, listApiKeys, listServiceAccounts, serviceAccountKeys, type ServiceAccountView,
} from "../../api";
import { Sel } from "../../components/controls";
import { ConfirmDialog } from "../../components/ConfirmDialog";
import { CreateSheet } from "../../components/CreateSheet";
import { EmptyState } from "../../components/EmptyState";
import { ErrorPanel } from "../../components/ErrorPanel";
import { FormField } from "../../components/FormField";
import { SkeletonTable } from "../../components/Skeleton";
import { Button } from "../../components/ui";
import { fieldErrors } from "../../errors";
import { useCreateParam, useLoad } from "../../hooks";
import { notify } from "../../toaster";
import { validate } from "../../validate";
import CreateKeyDialog from "./CreateKeyDialog";
import { Person } from "./common";
import { KeyTable } from "./KeyTable";
import { Badge } from "../../components/StatusBadge";
import { CardBody, HEAD_ROW, TableCard, Th } from "../../components/TableCard";
import { When } from "../../components/Time";

/** Service accounts: the identities that own API keys. Most developers never come here: "Create API key" makes one. */
export default function ServiceAccounts() {
  const list = useLoad(listServiceAccounts, "sas");
  const allKeys = useLoad(listApiKeys, "sas:keys");
  const [creating, setCreating] = useCreateParam();
  const [open, setOpen] = useState<string>();
  const current = list.data?.find((s) => s.id === open);
  const keyCount = useMemo(() => {
    const m = new Map<string, number>();
    for (const k of allKeys.data ?? []) if (!k.revoked && k.owner_type === "service_account") m.set(k.owner_id, (m.get(k.owner_id) ?? 0) + 1);
    return m;
  }, [allKeys.data]);
  const newButton = <Button tone="primary" onClick={() => setCreating(true)}>New service account</Button>;
  return (
    <div className="space-y-6">
      <TableCard
        title="Service accounts"
        count={list.data?.length}
        description="Identities for programs, not people. Each API key belongs to one: turn the account off or delete it and its keys stop working."
        actions={list.data?.length ? newButton : undefined}
      >
        {!!list.error && <CardBody><ErrorPanel error={list.error} onRetry={list.reload} /></CardBody>}
        {list.loading && !list.data && !list.error && <CardBody><SkeletonTable rows={3} columns={5} label="Loading service accounts" /></CardBody>}
        {list.data?.length === 0 && (
          <CardBody>
            <EmptyState
              title="No service accounts yet"
              why="A service account owns the API keys your apps use. Creating an API key makes one for you, or make one here first."
              action={newButton}
              icon={Robot01Icon}
              docs="docs/adr/0004-auth.md"
            />
          </CardBody>
        )}
        {!!list.data?.length && (
          <Table framed className="text-sm" aria-label="Service accounts">
            <Table.Header className={HEAD_ROW}>
              <Table.Row><Th>Name</Th><Th>Role</Th><Th align="end">Keys</Th><Th>Created</Th><Th>Status</Th><Th><span className="sr-only">Actions</span></Th></Table.Row>
            </Table.Header>
            <Table.Body>
              {list.data.map((s) => (
                <Table.Row key={s.id} aria-selected={open === s.id || undefined}>
                  <Table.Cell className="max-w-80">
                    <Person name={s.name} shape="square" fallback={<HugeiconsIcon icon={Robot01Icon} size={14} />} sub={s.description || undefined}>
                      {s.name}
                    </Person>
                  </Table.Cell>
                  <Table.Cell>{s.role === "admin" ? <Badge tone="warn" title="Can also manage the server">Admin</Badge> : <Badge tone="neutral">Service</Badge>}</Table.Cell>
                  <Table.Cell align="end">{allKeys.data ? keyCount.get(s.id) ?? 0 : "—"}</Table.Cell>
                  <Table.Cell className="text-muted-foreground"><When value={s.created_at} /></Table.Cell>
                  <Table.Cell>{s.disabled ? <Badge status="disabled">Disabled</Badge> : <Badge status="active">Active</Badge>}</Table.Cell>
                  <Table.Cell align="end">
                    <span className="inline-flex items-center gap-2">
                      <Button aria-label={`Keys of ${s.name}`} aria-expanded={open === s.id} onClick={() => setOpen(open === s.id ? undefined : s.id)}>
                        {open === s.id ? "Hide keys" : "Keys"}
                      </Button>
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
                          allKeys.reload();
                        }}
                      />
                    </span>
                  </Table.Cell>
                </Table.Row>
              ))}
            </Table.Body>
          </Table>
        )}
      </TableCard>
      {current && <Keys sa={current} onClose={() => setOpen(undefined)} onChanged={allKeys.reload} />}
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
        {(p) => <Sel {...p} size="md" className="w-full" value={role} onChange={setRole} options={[{ value: "service", label: "Service" }, { value: "admin", label: "Admin (can manage the server)" }]} />}
      </FormField>
    </CreateSheet>
  );
}

function Keys({ sa, onClose, onChanged }: { sa: ServiceAccountView; onClose: () => void; onChanged: () => void }) {
  const keys = useLoad((s) => serviceAccountKeys(sa.id, s), `sak:${sa.id}`);
  const [creating, setCreating] = useState(false);
  const changed = () => {
    keys.reload();
    onChanged();
  };
  return (
    <TableCard
      title={`Keys of ${sa.name}`}
      count={keys.data?.filter((k) => !k.revoked).length}
      description="Apps sign in as this account with these keys."
      actions={
        <>
          <Button onClick={onClose}>Close</Button>
          <Button tone="primary" onClick={() => setCreating(true)}>Create key</Button>
        </>
      }
    >
      {!!keys.error && <CardBody><ErrorPanel error={keys.error} onRetry={keys.reload} /></CardBody>}
      {keys.loading && !keys.data && !keys.error && <CardBody><SkeletonTable rows={2} columns={5} label="Loading keys" /></CardBody>}
      {keys.data?.length === 0 && <CardBody><EmptyState compact title="No keys yet" why="Create a key for this account so an app can sign in as it." /></CardBody>}
      {!!keys.data?.length && <KeyTable rows={keys.data} onRevoked={changed} />}
      <CreateKeyDialog open={creating} onOpenChange={setCreating} accountId={sa.id} onCreated={changed} />
    </TableCard>
  );
}
