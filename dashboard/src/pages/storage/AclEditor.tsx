import { useState } from "react";
import { PERMISSIONS, getBucketAcl, putBucketAcl } from "../../api";
import type { BucketAclEntry, Bucket, Permission } from "../../api";
import { EmptyState } from "../../components/EmptyState";
import { FormField } from "../../components/FormField";
import { HelpTip } from "../../components/HelpTip";
import { SkeletonTable } from "../../components/Skeleton";
import { Button, ErrorView, Panel } from "../../components/ui";
import { notify } from "../../toaster";
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
  const save = async (entries: BucketAclEntry[]) => {
    setError(undefined);
    try { await putBucketAcl(bucket.name, { entries }); notify.saved("Access list"); acl.reload(); } catch (e) { setError(e); }
  };
  const entries = acl.data?.entries ?? [];
  return (
    <div className="space-y-4">
      <Panel title="Access control list">
        <p className="mb-3 text-sm text-muted-foreground"><HelpTip term="acl">An access list</HelpTip> grants a user or role read, write or admin access to the keys under a prefix.</p>
        <ErrorView error={acl.error} />
        {acl.loading && !acl.error && <SkeletonTable rows={2} columns={4} label="Loading access list" />}
        {acl.data && entries.length === 0 && <EmptyState compact title="No entries yet" why="Only the owner and storage administrators have access. Add an entry below to let a user or role read or write." />}
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
        <ErrorView error={error} />
      </Panel>
      <Panel title="Add entry">
        <form className="flex flex-wrap items-end gap-3 text-sm" onSubmit={(e) => { e.preventDefault(); void save([...entries, { prefix, grantee: grantee.trim(), permission }]).then(() => { setPrefix(""); setGrantee(""); }); }}>
          <FormField label="Prefix" help="Empty covers keys in the whole bucket.">{(p) => <Input {...p} size="sm" className="w-40" placeholder="whole bucket" value={prefix} onChange={(e) => setPrefix(e.target.value)} />}</FormField>
          <FormField label="Grantee" required help="Such as user:alice, role:editor or *.">{(p) => <Input {...p} size="sm" className="w-44" placeholder="user:alice" value={grantee} onChange={(e) => setGrantee(e.target.value)} />}</FormField>
          <FormField label="Permission">{(p) => <Sel {...p} aria-label="Permission" value={permission} onChange={(v) => setPermission(v as Permission)} options={PERMISSIONS.map((x) => ({ value: x, label: x }))} />}</FormField>
          <Button type="submit" tone="primary" disabled={!grantee.trim()}>Add entry</Button>
        </form>
      </Panel>
    </div>
  );
}
