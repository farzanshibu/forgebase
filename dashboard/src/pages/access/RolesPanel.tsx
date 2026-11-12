import { Input } from "panelui-web/input";
import { Table } from "panelui-web/table";
import { useEffect, useRef, useState } from "react";
import { authzMe, deleteRole, listCollections, listRoles, putRole, type Role } from "../../api";
import { EmptyState } from "../../components/EmptyState";
import { ErrorPanel } from "../../components/ErrorPanel";
import { SkeletonTable } from "../../components/Skeleton";
import { Button, ConfirmButton } from "../../components/ui";
import { useLoad } from "../../hooks";
import { notify } from "../../toaster";
import PermissionMatrix from "./PermissionMatrix";
import { ActionError, Field, TextField } from "./shared";
import { useRun } from "./util";
import { Badge } from "../../components/StatusBadge";
import { CardBody, HEAD_ROW, TableCard, Th } from "../../components/TableCard";

/** Roles of an org (`org` set) or of the realm. Seeded roles are read-only unless the caller is a super-admin. */
export default function RolesPanel({ org, title = "Roles" }: { org?: string; title?: string }) {
  const key = `roles:${org ?? "realm"}`;
  const roles = useLoad((s) => listRoles(org, s), key);
  const me = useLoad((s) => authzMe(org, s), `me:${org ?? ""}`);
  const cols = useLoad(listCollections, "collections");
  const [editing, setEditing] = useState<string | "new" | null>(null);
  const { run, error } = useRun(() => {
    roles.reload();
    setEditing(null);
  });
  const superAdmin = !!me.data?.super_admin;
  const current = roles.data?.find((r) => r.key === editing);
  return (
    <div className="space-y-4">
      <TableCard
        title={title}
        count={roles.data?.length}
        description={org ? "Named sets of permissions you give to members and teams of this organization. Seeded roles come with the server." : "Roles that apply across the whole realm, given to accounts with a realm binding."}
        actions={<Button tone="primary" onClick={() => setEditing("new")}>New role</Button>}
      >
        {!!roles.error && <CardBody><ErrorPanel error={roles.error} onRetry={roles.reload} /></CardBody>}
        {roles.loading && !roles.data && !roles.error && <CardBody><SkeletonTable rows={4} columns={5} label="Loading roles" /></CardBody>}
        {roles.data?.length === 0 && <CardBody><EmptyState compact title="No roles yet" why="A role is a named set of permissions you give to members. Make one with New role." /></CardBody>}
        {!!roles.data?.length && (
          <Table framed className="text-sm" aria-label={title}>
            <Table.Header className={HEAD_ROW}>
              <Table.Row><Th>Role</Th><Th>Kind</Th><Th>Applies to</Th><Th align="end">Allows</Th><Th align="end">Denies</Th><Th><span className="sr-only">Actions</span></Th></Table.Row>
            </Table.Header>
            <Table.Body>
              {roles.data.map((r) => {
                const locked = (r.system && !superAdmin) || !r.assignable;
                return (
                  <Table.Row key={r.id} aria-selected={editing === r.key || undefined}>
                    <Table.Cell className="max-w-80">
                      <span className="flex min-w-0 flex-col">
                        <span className="truncate font-medium">{r.name}</span>
                        <code className="truncate font-mono text-xs text-muted-foreground">{r.key}</code>
                      </span>
                    </Table.Cell>
                    <Table.Cell>
                      <span className="flex flex-wrap gap-1">
                        {r.system ? <Badge tone="neutral" title="Comes with the server">Seeded</Badge> : <Badge>Custom</Badge>}
                        {!r.assignable && <Badge tone="warn" title="Cannot be given to members">Not assignable</Badge>}
                      </span>
                    </Table.Cell>
                    <Table.Cell className="text-muted-foreground">{r.org_id ? "This organization" : "Whole realm"}</Table.Cell>
                    <Table.Cell align="end">{r.allow.length}</Table.Cell>
                    <Table.Cell align="end" className={r.deny.length ? "font-medium text-destructive" : "text-muted-foreground"}>{r.deny.length}</Table.Cell>
                    <Table.Cell align="end">
                      <span className="inline-flex items-center gap-2">
                        <Button onClick={() => setEditing(r.key)}>{locked ? "View" : "Edit"}</Button>
                        {!r.system && (org ? r.org_id === org : !r.org_id) && (
                          <ConfirmButton label="Delete" consequence={`Role ${r.key} is removed. Members and teams that hold it lose its permissions.`} onConfirm={() => void run(() => deleteRole(org, r.key))} />
                        )}
                      </span>
                    </Table.Cell>
                  </Table.Row>
                );
              })}
            </Table.Body>
          </Table>
        )}
      </TableCard>
      <ActionError error={error} />
      {editing && (
        <RoleEditor
          key={editing}
          role={current}
          collections={(cols.data ?? []).filter((c) => !c.system && c.type !== "view").map((c) => c.name)}
          readOnly={!!current && ((current.system && !superAdmin) || !current.assignable)}
          onSave={(r) => void run(async () => { await putRole(org, r); notify.saved(`Role ${r.key}`); })}
          onCancel={() => setEditing(null)}
        />
      )}
    </div>
  );
}

function RoleEditor({ role, collections, readOnly, onSave, onCancel }: {
  role?: Role; collections: string[]; readOnly: boolean;
  onSave: (r: { key: string; name: string; description: string; allow: string[]; deny: string[] }) => void; onCancel: () => void;
}) {
  const [key, setKey] = useState(role?.key ?? "");
  const [name, setName] = useState(role?.name ?? "");
  const [description, setDescription] = useState(role?.description ?? "");
  const [allow, setAllow] = useState(role?.allow ?? []);
  const [deny, setDeny] = useState(role?.deny ?? []);
  const ref = useRef<HTMLElement>(null);
  useEffect(() => {
    ref.current?.scrollIntoView?.({ block: "start", behavior: "smooth" });
  }, []);
  return (
    <section ref={ref} aria-label={role ? `Role ${role.key}` : "New role"} className="scroll-mt-4 rounded-xl border border-primary/30 bg-card shadow-sm">
      <header className="flex flex-wrap items-start justify-between gap-3 border-b border-border px-4 py-3">
        <div>
          <h3 className="flex items-center gap-2 text-sm font-semibold">
            {role ? <>Role <code className="font-mono">{role.key}</code></> : "New role"}
            {readOnly && <Badge tone="neutral">Read-only</Badge>}
          </h3>
          <p className="mt-0.5 text-xs text-muted-foreground">
            {readOnly ? "Seeded roles can be changed by a super administrator only." : "Pick what the role allows and denies. A deny wins over an allow from any other role."}
          </p>
        </div>
      </header>
      <div className="space-y-4 px-4 py-4">
        <div className="grid gap-3 sm:grid-cols-3">
          {role ? (
            <Field label="Key" help="A role's key cannot change.">
              <Input size="sm" className="font-mono" value={key} readOnly disabled />
            </Field>
          ) : (
            <TextField label="Key" value={key} onChange={setKey} placeholder="support_agent" mono help="Letters, digits and underscores." />
          )}
          <TextField label="Name" value={name} onChange={setName} placeholder="Support agent" />
          <TextField label="Description" value={description} onChange={setDescription} placeholder="What the role is for" />
        </div>
        <PermissionMatrix allow={allow} deny={deny} collections={collections} readOnly={readOnly} onChange={(a, d) => { setAllow(a); setDeny(d); }} />
      </div>
      <footer className="flex justify-end gap-2 border-t border-border px-4 py-3">
        <Button onClick={onCancel}>Close</Button>
        {!readOnly && <Button tone="primary" disabled={!key} onClick={() => onSave({ key, name: name || key, description, allow, deny })}>Save role</Button>}
      </footer>
    </section>
  );
}
