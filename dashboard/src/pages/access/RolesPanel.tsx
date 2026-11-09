import { useState } from "react";
import { authzMe, deleteRole, listCollections, listRoles, putRole, type Role } from "../../api";
import { Button, ConfirmButton, ErrorView, Loading, Panel } from "../../components/ui";
import { useLoad } from "../../hooks";
import PermissionMatrix from "./PermissionMatrix";
import { ActionError, Empty, TextField } from "./shared";
import { useRun } from "./util";

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
      {roles.data && roles.data.length === 0 && <Empty>No roles.</Empty>}
      {roles.data && roles.data.length > 0 && (
        <table className="w-full text-left text-sm">
          <thead className="text-xs uppercase tracking-wide text-zinc-500">
            <tr><th className="py-2">Key</th><th>Name</th><th>Scope</th><th>Allow</th><th>Deny</th><th /></tr>
          </thead>
          <tbody>
            {roles.data.map((r) => {
              const locked = (r.system && !superAdmin) || !r.assignable;
              return (
                <tr key={r.id} className="border-t border-zinc-800">
                  <td className="py-2 font-mono">{r.key}</td>
                  <td>{r.name}{r.system && <span className="ml-2 text-xs text-zinc-500">seeded</span>}{!r.assignable && <span className="ml-2 text-xs text-zinc-500">not assignable</span>}</td>
                  <td>{r.org_id ? "org" : "realm"}</td>
                  <td>{r.allow.length}</td>
                  <td className={r.deny.length ? "text-red-300" : ""}>{r.deny.length}</td>
                  <td className="space-x-2 text-right">
                    <Button onClick={() => setEditing(r.key)}>{locked ? "View" : "Edit"}</Button>
                    {!r.system && (org ? r.org_id === org : !r.org_id) && <ConfirmButton label="Delete" onConfirm={() => void run(() => deleteRole(org, r.key))} />}
                  </td>
                </tr>
              );
            })}
          </tbody>
        </table>
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
