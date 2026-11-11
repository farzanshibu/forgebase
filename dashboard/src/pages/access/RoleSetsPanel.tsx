import { useState } from "react";
import { EmptyState } from "../../components/EmptyState";
import { listRoles, listRoleSets, putRoleSet } from "../../api";
import { Button, ErrorView, Loading, Panel } from "../../components/ui";
import { useLoad } from "../../hooks";
import { ActionError, MultiPick, TextField } from "./shared";
import { useRun } from "./util";
import { Table } from "panelui-web/table";

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
      <p className="text-sm text-muted-foreground">A role set bundles roles, so you can give them to an account together in one step.</p>
      <ErrorView error={sets.error} />
      {sets.loading && !sets.error && <Loading what="Loading role sets" />}
      {sets.data && sets.data.length === 0 && <EmptyState compact title="No role sets yet." why="A role set bundles roles so you can give them together in one step." />}
      {sets.data && sets.data.length > 0 && (
        <Table className="w-full text-left text-sm">
          <Table.Header className="text-xs uppercase tracking-wide text-muted-foreground"><Table.Row><Table.Head className="py-2">Key</Table.Head><Table.Head>Name</Table.Head><Table.Head>Roles</Table.Head><Table.Head /></Table.Row></Table.Header>
          <Table.Body>
            {sets.data.map((s) => (
              <Table.Row key={s.id} className="border-t border-border">
                <Table.Cell className="py-2 font-mono">{s.key}</Table.Cell><Table.Cell>{s.name}</Table.Cell><Table.Cell>{s.roles.join(", ")}</Table.Cell>
                <Table.Cell className="text-right"><Button onClick={() => edit(s.key)}>Edit {s.key}</Button></Table.Cell>
              </Table.Row>
            ))}
          </Table.Body>
        </Table>
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
