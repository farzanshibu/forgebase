import { useState } from "react";
import { listRoles, listRoleSets, putRoleSet } from "../../api";
import { Button, ErrorView, Loading, Panel } from "../../components/ui";
import { useLoad } from "../../hooks";
import { ActionError, Empty, MultiPick, TextField } from "./shared";
import { useRun } from "./util";

/** Role sets (named bundles of roles) of an org or of the realm. */
export default function RoleSetsPanel({ org }: { org?: string }) {
  const sets = useLoad((s) => listRoleSets(org, s), `sets:${org ?? "realm"}`);
  const roles = useLoad((s) => listRoles(org, s), `roles:${org ?? "realm"}`);
  const [key, setKey] = useState("");
  const [name, setName] = useState("");
  const [picked, setPicked] = useState<string[]>([]);
  const { run, error } = useRun(() => { sets.reload(); });
  const edit = (k: string) => {
    const s = sets.data?.find((x) => x.key === k);
    if (s) { setKey(s.key); setName(s.name); setPicked(s.roles); }
  };
  return (
    <div className="space-y-4">
      <ErrorView error={sets.error} />
      {sets.loading && !sets.error && <Loading what="Loading role sets" />}
      {sets.data && sets.data.length === 0 && <Empty>No role sets yet.</Empty>}
      {sets.data && sets.data.length > 0 && (
        <table className="w-full text-left text-sm">
          <thead className="text-xs uppercase tracking-wide text-zinc-500"><tr><th className="py-2">Key</th><th>Name</th><th>Roles</th><th /></tr></thead>
          <tbody>
            {sets.data.map((s) => (
              <tr key={s.id} className="border-t border-zinc-800">
                <td className="py-2 font-mono">{s.key}</td><td>{s.name}</td><td>{s.roles.join(", ")}</td>
                <td className="text-right"><Button onClick={() => edit(s.key)}>Edit {s.key}</Button></td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
      <Panel title="Create or replace a role set">
        <div className="grid gap-3 sm:grid-cols-2">
          <TextField label="Set key" value={key} onChange={setKey} />
          <TextField label="Set name" value={name} onChange={setName} />
        </div>
        <div className="mt-3"><MultiPick label="Roles in the set" options={(roles.data ?? []).filter((r) => r.assignable).map((r) => r.key)} value={picked} onChange={setPicked} /></div>
        <div className="mt-3"><Button tone="primary" disabled={!key} onClick={() => void run(() => putRoleSet(org, key, name || key, picked))}>Save role set</Button></div>
      </Panel>
      <ActionError error={error} />
    </div>
  );
}
