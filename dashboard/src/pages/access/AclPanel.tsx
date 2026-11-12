import { Table } from "panelui-web/table";
import { useCallback, useState } from "react";
import { deleteAcl, listAcl, putAcl, type AclEntry, type Effect } from "../../api";
import { Sel } from "../../components/controls";
import { EmptyState } from "../../components/EmptyState";
import { ErrorPanel } from "../../components/ErrorPanel";
import { HelpTip } from "../../components/HelpTip";
import { SkeletonTable } from "../../components/Skeleton";
import { Button, ConfirmButton } from "../../components/ui";
import { ActionError, EffectBadge, Field, FormCard, Principal, TextField } from "./shared";
import { rowCls, useRun } from "./util";
import { CardBody, HEAD_ROW, TableCard, Th } from "../../components/TableCard";

const TYPES = [
  { value: "project", label: "Project" },
  { value: "environment", label: "Environment" },
  { value: "collection", label: "Collection" },
];

/** Resource ACL of one project, environment or collection. */
export default function AclPanel() {
  const [type, setType] = useState("project");
  const [id, setId] = useState("");
  const [loaded, setLoaded] = useState<{ type: string; id: string }>();
  const [rows, setRows] = useState<AclEntry[]>();
  const [loadError, setLoadError] = useState<unknown>();
  const [grantee, setGrantee] = useState("");
  const [permission, setPermission] = useState("");
  const [effect, setEffect] = useState<Effect>("allow");

  const load = useCallback(async (t: string, i: string) => {
    setLoadError(undefined);
    try { setRows(await listAcl(t, i)); setLoaded({ type: t, id: i }); } catch (e) { setLoadError(e); setRows(undefined); }
  }, []);
  const { run, error, busy } = useRun(() => { if (loaded) void load(loaded.type, loaded.id); });

  return (
    <div className="space-y-4">
      <FormCard
        title={<span className="inline-flex items-center gap-1.5">Resource ACL <HelpTip term="acl" /></span>}
        description="An ACL lets one account or role use a single project, environment or collection, beyond what its roles allow. Pick the resource to see and change its entries."
      >
        <form className="flex flex-wrap items-end gap-3" onSubmit={(e) => { e.preventDefault(); if (id) void load(type, id); }}>
          <Field label="Resource type">
            <Sel value={type} onChange={setType} options={TYPES} className="min-w-36" />
          </Field>
          <TextField label="Resource id" value={id} onChange={setId} placeholder="prj_…" mono />
          <Button type="submit" disabled={!id}>Load ACL</Button>
        </form>
      </FormCard>
      {!!loadError && <ErrorPanel error={loadError} />}
      {loaded && (
        <TableCard
          title={<>Entries on {loaded.type} <code className="font-mono font-normal">{loaded.id}</code></>}
          aria-label="ACL entries"
          count={rows?.length}
        >
          {!rows && !loadError && <CardBody><SkeletonTable rows={2} columns={3} label="Loading ACL" /></CardBody>}
          {rows && rows.length === 0 && <CardBody><EmptyState compact title={`No ACL entries on ${loaded.type} ${loaded.id}`} why="An ACL entry lets one account or role use this one resource, beyond what its roles allow. Add one below." /></CardBody>}
          {rows && rows.length > 0 && (
            <Table framed className="text-sm" aria-label="ACL">
              <Table.Header className={HEAD_ROW}><Table.Row><Th>Grantee</Th><Th>Permission</Th><Th>Effect</Th><Th><span className="sr-only">Actions</span></Th></Table.Row></Table.Header>
              <Table.Body>
                {rows.map((a) => (
                  <Table.Row key={a.id} data-effect={a.effect} className={rowCls(a.effect === "deny")}>
                    <Table.Cell className="max-w-72"><Principal value={a.grantee} /></Table.Cell>
                    <Table.Cell><code className="font-mono text-xs">{a.permission}</code></Table.Cell>
                    <Table.Cell><EffectBadge effect={a.effect} /></Table.Cell>
                    <Table.Cell align="end"><ConfirmButton label="Remove" consequence={`${a.grantee} no longer gets ${a.permission} here from this entry.`} onConfirm={() => void run(() => deleteAcl(a.id))} /></Table.Cell>
                  </Table.Row>
                ))}
              </Table.Body>
            </Table>
          )}
        </TableCard>
      )}
      {loaded && (
        <FormCard title={`Add an entry to ${loaded.type} ${loaded.id}`}>
          <form className="flex flex-wrap items-end gap-3" onSubmit={(e) => { e.preventDefault(); if (grantee && permission) void run(() => putAcl({ resource_type: loaded.type, resource_id: loaded.id, grantee, permission, effect })); }}>
            <TextField label="Grantee" value={grantee} onChange={setGrantee} placeholder="user:users/abc" mono />
            <TextField label="ACL permission" value={permission} onChange={setPermission} placeholder="projects:read" mono />
            <Field label="ACL effect">
              <Sel value={effect} onChange={(v) => setEffect(v as Effect)} options={[{ value: "allow", label: "Allow" }, { value: "deny", label: "Deny" }]} className="min-w-28" />
            </Field>
            <Button type="submit" tone="primary" disabled={!grantee || !permission || busy}>Add ACL entry</Button>
          </form>
        </FormCard>
      )}
      <ActionError error={error} />
    </div>
  );
}
