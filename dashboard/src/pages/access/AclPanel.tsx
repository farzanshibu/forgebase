import { useCallback, useState } from "react";
import { deleteAcl, listAcl, putAcl, type AclEntry, type Effect } from "../../api";
import { Button, ConfirmButton, ErrorView, Loading, Panel } from "../../components/ui";
import { Sel } from "../../components/controls";
import { ActionError, Empty, EffectBadge, TextField } from "./shared";
import { rowCls, useRun } from "./util";
import { Table } from "panelui-web/table";

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
  const { run, error } = useRun(() => { if (loaded) void load(loaded.type, loaded.id); });

  return (
    <div className="space-y-4">
      <Panel title="Resource">
        <div className="flex flex-wrap items-end gap-3">
          <label className="flex flex-col gap-1 text-xs text-muted-foreground">Resource type
            <Sel value={type} onChange={setType} options={["project", "environment", "collection"]} />
          </label>
          <TextField label="Resource id" value={id} onChange={setId} />
          <Button disabled={!id} onClick={() => void load(type, id)}>Load ACL</Button>
        </div>
      </Panel>
      <ErrorView error={loadError} />
      {loaded && !rows && !loadError && <Loading what="Loading ACL" />}
      {rows && rows.length === 0 && <Empty>No ACL entries on {loaded?.type} {loaded?.id}.</Empty>}
      {rows && rows.length > 0 && (
        <Table className="w-full text-left text-sm">
          <Table.Header className="text-xs uppercase tracking-wide text-muted-foreground"><Table.Row><Table.Head className="py-2">Grantee</Table.Head><Table.Head>Permission</Table.Head><Table.Head>Effect</Table.Head><Table.Head /></Table.Row></Table.Header>
          <Table.Body>
            {rows.map((a) => (
              <Table.Row key={a.id} data-effect={a.effect} className={rowCls(a.effect === "deny")}>
                <Table.Cell className="py-2 font-mono">{a.grantee}</Table.Cell><Table.Cell className="font-mono">{a.permission}</Table.Cell><Table.Cell><EffectBadge effect={a.effect} /></Table.Cell>
                <Table.Cell className="text-right"><ConfirmButton label="Remove" onConfirm={() => void run(() => deleteAcl(a.id))} /></Table.Cell>
              </Table.Row>
            ))}
          </Table.Body>
        </Table>
      )}
      {loaded && (
        <Panel title={`Add entry to ${loaded.type} ${loaded.id}`}>
          <div className="flex flex-wrap items-end gap-3">
            <TextField label="Grantee" value={grantee} onChange={setGrantee} placeholder="user:users/abc" />
            <TextField label="ACL permission" value={permission} onChange={setPermission} placeholder="projects:read" />
            <label className="flex flex-col gap-1 text-xs text-muted-foreground">ACL effect
              <Sel value={effect} onChange={(v) => setEffect(v as Effect)} options={["allow", "deny"]} />
            </label>
            <Button tone="primary" disabled={!grantee || !permission} onClick={() => void run(() => putAcl({ resource_type: loaded.type, resource_id: loaded.id, grantee, permission, effect }))}>Add ACL entry</Button>
          </div>
        </Panel>
      )}
      <ActionError error={error} />
    </div>
  );
}
