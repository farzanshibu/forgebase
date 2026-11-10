import { useState } from "react";
import { PERMISSIONS, getBucketAcl, putBucketAcl } from "../../api";
import type { BucketAclEntry, Bucket, Permission } from "../../api";
import { Button, ErrorView, Loading, Panel } from "../../components/ui";
import { useLoad } from "../../hooks";
import { Table } from "panelui-web/table";
import { Input } from "panelui-web/input";
import { Sel } from "../../components/controls";

/** Prefix ACL: each entry grants a grantee (a principal such as `user:alice`, `role:editor` or `*`) a permission on keys under a prefix. */
export default function AclEditor({ bucket }: { bucket: Bucket }) {
  const acl = useLoad((s) => getBucketAcl(bucket.name, s), `acl:${bucket.name}`);
  const [prefix, setPrefix] = useState("");
  const [grantee, setGrantee] = useState("");
  const [permission, setPermission] = useState<Permission>("read");
  const [error, setError] = useState<unknown>();
  const [saved, setSaved] = useState(false);
  const save = async (entries: BucketAclEntry[]) => {
    setError(undefined); setSaved(false);
    try { await putBucketAcl(bucket.name, { entries }); setSaved(true); acl.reload(); } catch (e) { setError(e); }
  };
  const entries = acl.data?.entries ?? [];
  return (
    <div className="space-y-4">
      <Panel title="Access control list">
        <ErrorView error={acl.error} />
        {acl.loading && !acl.error && <Loading what="Loading ACL" />}
        {acl.data && entries.length === 0 && <p className="text-sm text-muted-foreground">No entries: only the owner and storage administrators have access.</p>}
        {entries.length > 0 && (
          <Table aria-label="ACL entries" className="w-full text-left text-sm">
            <Table.Header className="text-xs uppercase tracking-wide text-muted-foreground"><Table.Row><Table.Head className="py-1">Prefix</Table.Head><Table.Head>Grantee</Table.Head><Table.Head>Permission</Table.Head><Table.Head /></Table.Row></Table.Header>
            <Table.Body>
              {entries.map((e, i) => (
                <Table.Row key={`${e.prefix}|${e.grantee}|${i}`} className="border-t border-border">
                  <Table.Cell className="py-2 font-mono text-xs">{e.prefix || "(whole bucket)"}</Table.Cell><Table.Cell>{e.grantee}</Table.Cell><Table.Cell>{e.permission}</Table.Cell>
                  <Table.Cell className="text-right"><Button tone="danger" aria-label={`Remove ${e.grantee} on ${e.prefix || "bucket"}`} onClick={() => void save(entries.filter((_, j) => j !== i))}>Remove</Button></Table.Cell>
                </Table.Row>
              ))}
            </Table.Body>
          </Table>
        )}
        {saved && <p role="status" className="mt-2 text-sm text-success">ACL saved.</p>}
        <ErrorView error={error} />
      </Panel>
      <Panel title="Add entry">
        <form className="flex flex-wrap items-end gap-3 text-sm" onSubmit={(e) => { e.preventDefault(); void save([...entries, { prefix, grantee: grantee.trim(), permission }]).then(() => { setPrefix(""); setGrantee(""); }); }}>
          <label>Prefix<Input size="sm" className="ml-2 w-40" placeholder="whole bucket" value={prefix} onChange={(e) => setPrefix(e.target.value)} /></label>
          <label>Grantee<Input size="sm" className="ml-2 w-44" placeholder="user:alice" value={grantee} onChange={(e) => setGrantee(e.target.value)} required /></label>
          <label>Permission
            <Sel aria-label="Permission" className="ml-2" value={permission} onChange={(v) => setPermission(v as Permission)} options={PERMISSIONS.map((p) => ({ value: p, label: p }))} />
          </label>
          <Button type="submit" tone="primary" disabled={!grantee.trim()}>Add entry</Button>
        </form>
      </Panel>
    </div>
  );
}
