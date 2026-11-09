import { useState } from "react";
import { PERMISSIONS, getAcl, putAcl } from "../../api";
import type { AclEntry, Bucket, Permission } from "../../api";
import { Button, ErrorView, Loading, Panel, inputCls } from "../../components/ui";
import { useLoad } from "../../hooks";

/** Prefix ACL: each entry grants a grantee (a principal such as `user:alice`, `role:editor` or `*`) a permission on keys under a prefix. */
export default function AclEditor({ bucket }: { bucket: Bucket }) {
  const acl = useLoad((s) => getAcl(bucket.name, s), `acl:${bucket.name}`);
  const [prefix, setPrefix] = useState("");
  const [grantee, setGrantee] = useState("");
  const [permission, setPermission] = useState<Permission>("read");
  const [error, setError] = useState<unknown>();
  const [saved, setSaved] = useState(false);
  const save = async (entries: AclEntry[]) => {
    setError(undefined); setSaved(false);
    try { await putAcl(bucket.name, { entries }); setSaved(true); acl.reload(); } catch (e) { setError(e); }
  };
  const entries = acl.data?.entries ?? [];
  return (
    <div className="space-y-4">
      <Panel title="Access control list">
        <ErrorView error={acl.error} />
        {acl.loading && !acl.error && <Loading what="Loading ACL" />}
        {acl.data && entries.length === 0 && <p className="text-sm text-zinc-400">No entries: only the owner and storage administrators have access.</p>}
        {entries.length > 0 && (
          <table aria-label="ACL entries" className="w-full text-left text-sm">
            <thead className="text-xs uppercase tracking-wide text-zinc-500"><tr><th className="py-1">Prefix</th><th>Grantee</th><th>Permission</th><th /></tr></thead>
            <tbody>
              {entries.map((e, i) => (
                <tr key={`${e.prefix}|${e.grantee}|${i}`} className="border-t border-zinc-800">
                  <td className="py-2 font-mono text-xs">{e.prefix || "(whole bucket)"}</td><td>{e.grantee}</td><td>{e.permission}</td>
                  <td className="text-right"><Button tone="danger" aria-label={`Remove ${e.grantee} on ${e.prefix || "bucket"}`} onClick={() => void save(entries.filter((_, j) => j !== i))}>Remove</Button></td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
        {saved && <p role="status" className="mt-2 text-sm text-emerald-300">ACL saved.</p>}
        <ErrorView error={error} />
      </Panel>
      <Panel title="Add entry">
        <form className="flex flex-wrap items-end gap-3 text-sm" onSubmit={(e) => { e.preventDefault(); void save([...entries, { prefix, grantee: grantee.trim(), permission }]).then(() => { setPrefix(""); setGrantee(""); }); }}>
          <label>Prefix<input className={`${inputCls} ml-2 w-40`} placeholder="whole bucket" value={prefix} onChange={(e) => setPrefix(e.target.value)} /></label>
          <label>Grantee<input className={`${inputCls} ml-2 w-44`} placeholder="user:alice" value={grantee} onChange={(e) => setGrantee(e.target.value)} required /></label>
          <label>Permission
            <select className={`${inputCls} ml-2`} value={permission} onChange={(e) => setPermission(e.target.value as Permission)}>{PERMISSIONS.map((p) => <option key={p}>{p}</option>)}</select>
          </label>
          <Button type="submit" tone="primary" disabled={!grantee.trim()}>Add entry</Button>
        </form>
      </Panel>
    </div>
  );
}
