import { useCallback, useState } from "react";
import { deleteAcl, listAcl, putAcl, type AclEntry, type Effect } from "../../api";
import { Button, ConfirmButton, ErrorView, Loading, Panel, inputCls } from "../../components/ui";
import { ActionError, Empty, EffectBadge, TextField } from "./shared";
import { rowCls, useRun } from "./util";

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
          <label className="flex flex-col gap-1 text-xs text-zinc-400">Resource type
            <select className={inputCls} value={type} onChange={(e) => setType(e.target.value)}>
              <option value="project">project</option><option value="environment">environment</option><option value="collection">collection</option>
            </select>
          </label>
          <TextField label="Resource id" value={id} onChange={setId} />
          <Button disabled={!id} onClick={() => void load(type, id)}>Load ACL</Button>
        </div>
      </Panel>
      <ErrorView error={loadError} />
      {loaded && !rows && !loadError && <Loading what="Loading ACL" />}
      {rows && rows.length === 0 && <Empty>No ACL entries on {loaded?.type} {loaded?.id}.</Empty>}
      {rows && rows.length > 0 && (
        <table className="w-full text-left text-sm">
          <thead className="text-xs uppercase tracking-wide text-zinc-500"><tr><th className="py-2">Grantee</th><th>Permission</th><th>Effect</th><th /></tr></thead>
          <tbody>
            {rows.map((a) => (
              <tr key={a.id} data-effect={a.effect} className={rowCls(a.effect === "deny")}>
                <td className="py-2 font-mono">{a.grantee}</td><td className="font-mono">{a.permission}</td><td><EffectBadge effect={a.effect} /></td>
                <td className="text-right"><ConfirmButton label="Remove" onConfirm={() => void run(() => deleteAcl(a.id))} /></td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
      {loaded && (
        <Panel title={`Add entry to ${loaded.type} ${loaded.id}`}>
          <div className="flex flex-wrap items-end gap-3">
            <TextField label="Grantee" value={grantee} onChange={setGrantee} placeholder="user:users/abc" />
            <TextField label="ACL permission" value={permission} onChange={setPermission} placeholder="projects:read" />
            <label className="flex flex-col gap-1 text-xs text-zinc-400">ACL effect
              <select className={inputCls} value={effect} onChange={(e) => setEffect(e.target.value as Effect)}><option value="allow">allow</option><option value="deny">deny</option></select>
            </label>
            <Button tone="primary" disabled={!grantee || !permission} onClick={() => void run(() => putAcl({ resource_type: loaded.type, resource_id: loaded.id, grantee, permission, effect }))}>Add ACL entry</Button>
          </div>
        </Panel>
      )}
      <ActionError error={error} />
    </div>
  );
}
