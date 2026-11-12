import { Table } from "panelui-web/table";
import { useState } from "react";
import { listRoles, listRoleSets, putRoleSet } from "../../api";
import { EmptyState } from "../../components/EmptyState";
import { ErrorPanel } from "../../components/ErrorPanel";
import { HelpTip } from "../../components/HelpTip";
import { SkeletonTable } from "../../components/Skeleton";
import { Button } from "../../components/ui";
import { useLoad } from "../../hooks";
import { notify } from "../../toaster";
import { ActionError, FormCard, MultiPick, TextField } from "./shared";
import { useRun } from "./util";
import { CardBody, HEAD_ROW, TableCard, Th } from "../../components/TableCard";
import { Chips } from "../../components/Chips";

/** Role sets (named bundles of roles) of an org or of the realm. */
export default function RoleSetsPanel({ org, title = "Role sets" }: { org?: string; title?: string }) {
  const sets = useLoad((s) => listRoleSets(org, s), `sets:${org ?? "realm"}`);
  const roles = useLoad((s) => listRoles(org, s), `roles:${org ?? "realm"}`);
  const [key, setKey] = useState("");
  const [name, setName] = useState("");
  const [picked, setPicked] = useState<string[]>([]);
  const existing = sets.data?.some((x) => x.key === key);
  const reset = () => { setKey(""); setName(""); setPicked([]); };
  const { run, error, busy } = useRun(() => { sets.reload(); });
  const edit = (k: string) => {
    const s = sets.data?.find((x) => x.key === k);
    if (s) { setKey(s.key); setName(s.name); setPicked(s.roles); }
  };
  return (
    <div className="space-y-4">
      <TableCard
        title={<span className="inline-flex items-center gap-1.5">{title} <HelpTip term="role_set" /></span>}
        aria-label={title}
        count={sets.data?.length}
        description="A role set bundles roles, so you can give them to an account together in one step."
      >
        {!!sets.error && <CardBody><ErrorPanel error={sets.error} onRetry={sets.reload} /></CardBody>}
        {sets.loading && !sets.data && !sets.error && <CardBody><SkeletonTable rows={2} columns={3} label="Loading role sets" /></CardBody>}
        {sets.data?.length === 0 && <CardBody><EmptyState compact title="No role sets yet" why="Make one below, for example “staff” holding the support and analyst roles." /></CardBody>}
        {!!sets.data?.length && (
          <Table framed className="text-sm" aria-label={`${title} list`}>
            <Table.Header className={HEAD_ROW}><Table.Row><Th>Role set</Th><Th>Roles in it</Th><Th><span className="sr-only">Actions</span></Th></Table.Row></Table.Header>
            <Table.Body>
              {sets.data.map((s) => (
                <Table.Row key={s.id} aria-selected={key === s.key || undefined}>
                  <Table.Cell>
                    <span className="flex flex-col">
                      <span className="font-medium">{s.name}</span>
                      <code className="font-mono text-xs text-muted-foreground">{s.key}</code>
                    </span>
                  </Table.Cell>
                  <Table.Cell><Chips items={s.roles} max={5} /></Table.Cell>
                  <Table.Cell align="end"><Button aria-label={`Edit ${s.key}`} onClick={() => edit(s.key)}>Edit</Button></Table.Cell>
                </Table.Row>
              ))}
            </Table.Body>
          </Table>
        )}
      </TableCard>
      <FormCard
        title={existing ? `Change role set ${key}` : "New role set"}
        description="Saving a key that already exists replaces that set."
        footer={
          <>
            {(key || name || picked.length > 0) && <Button onClick={reset}>Clear</Button>}
            <Button tone="primary" disabled={!key || busy} onClick={() => void run(async () => { await putRoleSet(org, key, name || key, picked); notify.saved(`Role set ${key}`); })}>Save role set</Button>
          </>
        }
      >
        <div className="grid gap-3 sm:grid-cols-2">
          <TextField label="Set key" value={key} onChange={setKey} placeholder="staff" mono />
          <TextField label="Set name" value={name} onChange={setName} placeholder="Staff" />
        </div>
        <MultiPick label="Roles in the set" options={(roles.data ?? []).filter((r) => r.assignable).map((r) => r.key)} value={picked} onChange={setPicked} />
      </FormCard>
      <ActionError error={error} />
    </div>
  );
}
