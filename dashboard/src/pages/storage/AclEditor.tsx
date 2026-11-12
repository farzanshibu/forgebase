import { useState } from "react";
import { PERMISSIONS, getBucketAcl, putBucketAcl } from "../../api";
import type { BucketAclEntry, Bucket, Permission } from "../../api";
import { EmptyState } from "../../components/EmptyState";
import { FormField } from "../../components/FormField";
import { HelpTip } from "../../components/HelpTip";
import { SkeletonTable } from "../../components/Skeleton";
import { Button, ErrorView } from "../../components/ui";
import { Delete02Icon } from "@hugeicons/core-free-icons";
import { Badge } from "panelui-web/badge";
import { IconButton, TableCard } from "../database/parts";
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
    <div className="max-w-4xl space-y-4">
      <TableCard
        title="Access control list"
        count={acl.data ? entries.length : undefined}
        description={<><HelpTip term="acl">An access list</HelpTip> grants a user or role read, write or admin access to the keys under a prefix.</>}
      >
        {acl.error !== undefined && <div className="p-4"><ErrorView error={acl.error} onRetry={acl.reload} /></div>}
        {acl.loading && !acl.error && <div className="p-4"><SkeletonTable rows={2} columns={4} label="Loading access list" /></div>}
        {acl.data && entries.length === 0 && <div className="p-4"><EmptyState compact title="No entries yet" why="Only the owner and storage administrators have access. Add an entry below to let a user or role read or write." /></div>}
        {entries.length > 0 && (
          <Table framed density="compact" aria-label="ACL entries">
            <Table.Header className="bg-muted/40"><Table.Row><Table.Head>Prefix</Table.Head><Table.Head>Grantee</Table.Head><Table.Head>Permission</Table.Head><Table.Head align="end"><span className="sr-only">Actions</span></Table.Head></Table.Row></Table.Header>
            <Table.Body>
              {entries.map((e, i) => (
                <Table.Row key={`${e.prefix}|${e.grantee}|${i}`}>
                  <Table.Cell className="py-2 font-mono text-xs">{e.prefix || <span className="font-sans text-muted-foreground">Whole bucket</span>}</Table.Cell>
                  <Table.Cell className="py-2 font-mono text-xs">{e.grantee}</Table.Cell>
                  <Table.Cell className="py-2"><Badge variant={e.permission === "admin" ? "warning" : e.permission === "write" ? "info" : "secondary"} size="sm">{e.permission}</Badge></Table.Cell>
                  <Table.Cell align="end" className="py-2"><IconButton icon={Delete02Icon} tone="danger" label={`Remove ${e.grantee} on ${e.prefix || "bucket"}`} onClick={() => void save(entries.filter((_, j) => j !== i))} /></Table.Cell>
                </Table.Row>
              ))}
            </Table.Body>
          </Table>
        )}
        {error !== undefined && <div className="border-t border-border p-4"><ErrorView error={error} /></div>}
      </TableCard>
      <section aria-label="Add entry" className="rounded-xl border border-border bg-card p-5">
        <h2 className="mb-4 text-sm font-medium">Add entry</h2>
        <form className="grid gap-4 text-sm sm:grid-cols-[1fr_1fr_10rem_auto] sm:items-start" onSubmit={(e) => { e.preventDefault(); void save([...entries, { prefix, grantee: grantee.trim(), permission }]).then(() => { setPrefix(""); setGrantee(""); }); }}>
          <FormField label="Prefix" help="Empty covers the whole bucket.">{(p) => <Input {...p} size="sm" className="font-mono" placeholder="photos/" value={prefix} onChange={(e) => setPrefix(e.target.value)} />}</FormField>
          <FormField label="Grantee" required help="Such as user:alice, role:editor or *.">{(p) => <Input {...p} size="sm" className="font-mono" placeholder="user:alice" value={grantee} onChange={(e) => setGrantee(e.target.value)} />}</FormField>
          <FormField label="Permission">{(p) => <Sel {...p} aria-label="Permission" className="w-full" value={permission} onChange={(v) => setPermission(v as Permission)} options={PERMISSIONS.map((x) => ({ value: x, label: x }))} />}</FormField>
          <div className="sm:pt-6"><Button type="submit" tone="primary" disabled={!grantee.trim()}>Add entry</Button></div>
        </form>
      </section>
    </div>
  );
}
