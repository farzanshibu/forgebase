import { useState } from "react";
import { EmptyState } from "../../components/EmptyState";
import { authzMe, deleteRole, listCollections, listRoles, putRole, type Role } from "../../api";
import { Button, ConfirmButton, ErrorView, Loading, Panel } from "../../components/ui";
import { useLoad } from "../../hooks";
import PermissionMatrix from "./PermissionMatrix";
import { ActionError, TextField } from "./shared";
import { useRun } from "./util";
import { Table } from "panelui-web/table";

/** Roles of an org (`org` set) or of the realm. Seeded roles are read-only unless the caller is a super-admin. */
export default function RolesPanel({ org }: { org?: string }) {
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
      <ErrorView error={roles.error} />
      {roles.loading && !roles.error && <Loading what="Loading roles" />}
      {roles.data && roles.data.length === 0 && <EmptyState compact title="No roles." why="A role is a named set of permissions you give to members. Add one below." />}
      {roles.data && roles.data.length > 0 && (
        <Table className="w-full text-left text-sm">
          <Table.Header className="text-xs uppercase tracking-wide text-muted-foreground">
            <Table.Row><Table.Head className="py-2">Key</Table.Head><Table.Head>Name</Table.Head><Table.Head>Scope</Table.Head><Table.Head>Allow</Table.Head><Table.Head>Deny</Table.Head><Table.Head /></Table.Row>
          </Table.Header>
          <Table.Body>
            {roles.data.map((r) => {
              const locked = (r.system && !superAdmin) || !r.assignable;
              return (
                <Table.Row key={r.id} className="border-t border-border">
                  <Table.Cell className="py-2 font-mono">{r.key}</Table.Cell>
                  <Table.Cell>{r.name}{r.system && <span className="ml-2 text-xs text-muted-foreground">seeded</span>}{!r.assignable && <span className="ml-2 text-xs text-muted-foreground">not assignable</span>}</Table.Cell>
                  <Table.Cell>{r.org_id ? "org" : "realm"}</Table.Cell>
                  <Table.Cell>{r.allow.length}</Table.Cell>
                  <Table.Cell className={r.deny.length ? "text-destructive" : ""}>{r.deny.length}</Table.Cell>
                  <Table.Cell className="space-x-2 text-right">
                    <Button onClick={() => setEditing(r.key)}>{locked ? "View" : "Edit"}</Button>
                    {!r.system && (org ? r.org_id === org : !r.org_id) && <ConfirmButton label="Delete" onConfirm={() => void run(() => deleteRole(org, r.key))} />}
                  </Table.Cell>
                </Table.Row>
              );
            })}
          </Table.Body>
        </Table>
      )}
      <Button tone="primary" onClick={() => setEditing("new")}>New role</Button>
      <ActionError error={error} />
      {editing && (
        <RoleEditor
          key={editing}
          role={current}
          collections={(cols.data ?? []).filter((c) => !c.system && c.type !== "view").map((c) => c.name)}
          readOnly={!!current && ((current.system && !superAdmin) || !current.assignable)}
          onSave={(r) => void run(() => putRole(org, r))}
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
  return (
    <Panel title={role ? `Role ${role.key}${readOnly ? " (read-only)" : ""}` : "New role"}>
      <div className="mb-3 grid gap-3 sm:grid-cols-3">
        <TextField label="Key" value={key} onChange={role ? () => {} : setKey} placeholder="support_agent" />
        <TextField label="Name" value={name} onChange={setName} />
        <TextField label="Description" value={description} onChange={setDescription} />
      </div>
      <PermissionMatrix allow={allow} deny={deny} collections={collections} readOnly={readOnly} onChange={(a, d) => { setAllow(a); setDeny(d); }} />
      <div className="mt-3 flex gap-2">
        {!readOnly && <Button tone="primary" onClick={() => onSave({ key, name: name || key, description, allow, deny })}>Save role</Button>}
        <Button onClick={onCancel}>Close</Button>
      </div>
    </Panel>
  );
}
