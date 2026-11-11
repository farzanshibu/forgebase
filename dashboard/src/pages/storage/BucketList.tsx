import { useState } from "react";
import { Link } from "react-router-dom";
import { Input } from "panelui-web/input";
import { Table } from "panelui-web/table";
import { ApiError, VISIBILITIES, bucketUsage, createBucket, deleteBucket, listBuckets, reconcileStorage, rotateSigningKey, storageQuotaFor, storageStats } from "../../api";
import type { Bucket, ReconcileReport, Visibility } from "../../api";
import { useSession } from "../../auth/session";
import { Check, Sel } from "../../components/controls";
import { ConfirmDialog } from "../../components/ConfirmDialog";
import { CreateSheet } from "../../components/CreateSheet";
import { EmptyState } from "../../components/EmptyState";
import { fieldErrors } from "../../components/ErrorPanel";
import { FormField } from "../../components/FormField";
import { PageHeader } from "../../components/PageHeader";
import { SkeletonTable } from "../../components/Skeleton";
import { Button, ErrorView, Panel } from "../../components/ui";
import { formatBytes } from "../../format";
import { useCreateParam, useLoad } from "../../hooks";
import { notify } from "../../toaster";
import { validate } from "../../validate";
import { when } from "../functions/util";
import { parseBytes } from "./util";

/** Where a bucket lives: one tenant, or the whole instance (FB-140: a tenant's buckets are stored as `<tenant>/<name>`). */
export const bucketScope = (b: Bucket) => (b.tenant ? { label: b.tenant, text: `Tenant ${b.tenant}` } : { label: "Instance", text: "Instance-wide: every tenant's rules apply to it" });

export default function BucketList() {
  const buckets = useLoad(listBuckets, "buckets");
  const [creating, setCreating] = useCreateParam();
  const newBucket = <Button tone="primary" onClick={() => setCreating(true)}>New bucket</Button>;
  return (
    <div className="space-y-4">
      <PageHeader studio="storage" primary={newBucket} />
      <Usage />
      <Panel title="Buckets">
        <ErrorView error={buckets.error} onRetry={buckets.reload} />
        {buckets.loading && !buckets.error && <SkeletonTable rows={3} columns={6} label="Loading buckets" />}
        {buckets.data && buckets.data.length === 0 && (
          <EmptyState
            title="No buckets yet"
            why="Buckets hold the files your app uploads, like avatars or exports. Create one, then drop files into it or upload from your code."
            action={newBucket}
            docs="docs/adr/0018-object-storage.md"
          />
        )}
        {buckets.data && buckets.data.length > 0 && (
          <Table aria-label="Buckets" className="w-full text-left text-sm">
            <Table.Header className="text-xs uppercase tracking-wide text-muted-foreground"><Table.Row><Table.Head className="py-1">Name</Table.Head><Table.Head>Scope</Table.Head><Table.Head>Visibility</Table.Head><Table.Head>Versioning</Table.Head><Table.Head>Max object</Table.Head><Table.Head>Used</Table.Head><Table.Head>Created</Table.Head><Table.Head><span className="sr-only">Actions</span></Table.Head></Table.Row></Table.Header>
            <Table.Body>
              {buckets.data.map((b) => (
                <Table.Row key={b.name} className="border-t border-border">
                  <Table.Cell className="py-2"><Link className="text-primary hover:underline" to={`/storage/b/${encodeURIComponent(b.name)}`}>{b.name}</Link></Table.Cell>
                  <Table.Cell title={bucketScope(b).text}>{bucketScope(b).label}</Table.Cell>
                  <Table.Cell>{b.visibility}</Table.Cell><Table.Cell>{b.versioning ? "on" : "off"}</Table.Cell>
                  <Table.Cell>{b.max_object_bytes ? formatBytes(b.max_object_bytes) : "no limit"}</Table.Cell>
                  <Table.Cell><BucketUsageCell name={b.name} /></Table.Cell>
                  <Table.Cell className="text-muted-foreground">{when(b.created_at)}</Table.Cell>
                  <Table.Cell className="text-right"><DeleteBucket name={b.name} onDeleted={buckets.reload} /></Table.Cell>
                </Table.Row>
              ))}
            </Table.Body>
          </Table>
        )}
      </Panel>
      <NewBucketSheet open={creating} onOpenChange={setCreating} onCreated={buckets.reload} />
    </div>
  );
}

/** Delete one bucket. "Also delete its objects" is part of this bucket's dialog, not a page-wide switch (A-13). */
function DeleteBucket({ name, onDeleted }: { name: string; onDeleted: () => void }) {
  const [alsoObjects, setAlsoObjects] = useState(false);
  return (
    <ConfirmDialog
      trigger={<Button tone="danger" aria-label={`Delete bucket ${name}`}>Delete</Button>}
      title={`Delete bucket ${name}?`}
      consequence={alsoObjects ? "Every file in it and every earlier version is deleted. Apps that read them get 404. This cannot be undone." : "The bucket is deleted only if it is empty. Apps that use it get 404."}
      confirmLabel="Delete bucket"
      confirmText={name}
      onConfirm={async () => {
        await deleteBucket(name, alsoObjects);
        notify.deleted(`Bucket ${name}`);
        onDeleted();
      }}
    >
      <Check checked={alsoObjects} onChange={setAlsoObjects} label="Also delete its objects" description="Without this, a bucket that still holds files cannot be deleted." />
    </ConfirmDialog>
  );
}

/** Objects and bytes of one bucket from `GET .../buckets/{bucket}/usage`; a refusal shows "n/a". */
function BucketUsageCell({ name }: { name: string }) {
  const u = useLoad((s) => bucketUsage(name, s), `bucket-usage:${name}`);
  if (u.error) return <span className="text-muted-foreground">n/a</span>;
  if (!u.data) return <span className="text-muted-foreground">…</span>;
  return <span aria-label={`Usage of ${name}`}>{formatBytes(u.data.logical_bytes)} · {u.data.objects} object{u.data.objects === 1 ? "" : "s"}</span>;
}

function NewBucketSheet({ open, onOpenChange, onCreated }: { open: boolean; onOpenChange: (o: boolean) => void; onCreated: () => void }) {
  const [name, setName] = useState("");
  const [touched, setTouched] = useState(false);
  const [visibility, setVisibility] = useState<Visibility>("private");
  const [versioning, setVersioning] = useState(false);
  const [max, setMax] = useState("");
  const [tenant, setTenant] = useState("");
  const [saveError, setSaveError] = useState<unknown>();
  const maxError = (() => { try { parseBytes(max); return undefined; } catch (e) { return (e as Error).message; } })();
  const nameError = (touched ? validate.bucketName(name) : undefined) ?? fieldErrors(saveError).name;
  return (
    <CreateSheet
      open={open}
      onOpenChange={onOpenChange}
      title="New bucket"
      description="Buckets hold the files your app uploads."
      submitLabel="Create bucket"
      canSubmit={!validate.bucketName(name) && !maxError}
      onError={setSaveError}
      onSubmit={async () => {
        const b = name.trim();
        await createBucket({ name: b, visibility, versioning, max_object_bytes: parseBytes(max) }, tenant.trim() || undefined);
        onCreated();
        setName(""); setMax(""); setTenant(""); setTouched(false); setSaveError(undefined);
        return { what: `Bucket ${b}`, href: `/storage/b/${encodeURIComponent(tenant.trim() ? `${tenant.trim()}/${b}` : b)}` };
      }}
    >
      <FormField label="Name" required error={nameError} help="Lower-case letters, digits, dots and dashes.">
        {(p) => <Input {...p} size="sm" value={name} onChange={(e) => { setName(e.target.value); setTouched(true); setSaveError(undefined); }} />}
      </FormField>
      <FormField label="Visibility" help="Private: the owner and people you grant access. Authenticated: any signed-in caller can read. Public: anyone can read.">
        {(p) => <Sel {...p} aria-label="Visibility" value={visibility} onChange={(v) => setVisibility(v as Visibility)} options={VISIBILITIES.map((v) => ({ value: v, label: v }))} />}
      </FormField>
      <Check checked={versioning} onChange={setVersioning} label="Versioning" description="Keep earlier versions of each file." />
      <FormField label="Max object size" error={maxError} help="For example 10 MiB. Leave empty for no limit.">
        {(p) => <Input {...p} size="sm" placeholder="no limit" value={max} onChange={(e) => setMax(e.target.value)} />}
      </FormField>
      <FormField label="Tenant" help="Optional. Creates the bucket for one tenant, stored as <tenant>/<name>. Leave empty for an instance-wide bucket.">
        {(p) => <Input {...p} size="sm" value={tenant} onChange={(e) => setTenant(e.target.value)} />}
      </FormField>
    </CreateSheet>
  );
}

/**
 * Instance usage against the `storage.bytes` entitlement, plus the reconciler and the signing key. These need
 * administrator rights; a refusal is shown, not fatal. Entitlements belong to an organization or a signed-in user, so
 * an administrator's session asks for none (A-21).
 */
function Usage() {
  const session = useSession();
  const subject = session.admin && !session.impersonation ? null : "caller";
  const stats = useLoad(storageStats, "storage-stats");
  const quota = useLoad((s) => storageQuotaFor(subject, s).catch(() => null), `storage-quota:${subject ?? "none"}`);
  const [report, setReport] = useState<ReconcileReport>();
  const [error, setError] = useState<unknown>();
  const [busy, setBusy] = useState(false);
  const run = async () => {
    setBusy(true); setError(undefined);
    try { setReport(await reconcileStorage()); stats.reload(); } catch (e) { setError(e); } finally { setBusy(false); }
  };
  const used = stats.data?.logical_bytes ?? 0;
  const limit = quota.data ?? null;
  const pct = limit ? Math.min(100, Math.round((used / limit) * 100)) : 0;
  const tenantScope = stats.data?.scope === "tenant";
  return (
    <Panel
      title="Usage"
      actions={
        <>
          <Button onClick={() => void run()} disabled={busy}>Run reconcile</Button>
          <ConfirmDialog
            trigger={<Button>Rotate signing key</Button>}
            title="Rotate the signing key?"
            consequence="Every signed URL issued so far stops working at once, including the ones apps already handed out. New URLs work as usual."
            confirmLabel="Rotate key"
            onConfirm={async () => { await rotateSigningKey(); notify.saved("Signing key"); }}
          />
        </>
      }
    >
      {stats.error instanceof ApiError && stats.error.isAuth ? <p className="text-sm text-muted-foreground">Instance totals need the storage_admin permission.</p> : <ErrorView error={stats.error} />}
      {stats.data && (
        <div className="space-y-2 text-sm">
          <p aria-label="Storage totals" className="text-foreground/80">
            {tenantScope && <span className="mr-2 rounded bg-muted px-1.5 py-0.5 text-xs">Tenant {stats.data.tenant ?? ""} only</span>}
            {stats.data.buckets} buckets · {stats.data.objects} objects · {stats.data.versions} versions · {formatBytes(stats.data.logical_bytes)} logical · {formatBytes(stats.data.stored_bytes)} stored · {stats.data.open_uploads} open uploads
          </p>
          {limit !== null ? (
            <div>
              <p aria-label="Quota usage" className="text-muted-foreground">{formatBytes(used)} of {formatBytes(limit)} used ({pct}%)</p>
              <div role="progressbar" aria-label="Storage quota" aria-valuenow={pct} aria-valuemin={0} aria-valuemax={100} className="mt-1 h-2 w-full overflow-hidden rounded bg-muted">
                <div className={`h-full ${pct >= 90 ? "bg-destructive" : pct >= 70 ? "bg-primary" : "bg-success"}`} style={{ width: `${pct}%` }} />
              </div>
            </div>
          ) : (
            <p className="text-muted-foreground">
              {subject === null ? "No quota applies to administrators. " : "No quota set for this account. "}
              A quota belongs to an organization or user: set it in <Link className="text-primary hover:underline" to="/billing">Billing</Link>.
            </p>
          )}
        </div>
      )}
      <ErrorView error={error} />
      {report && (
        <p aria-label="Reconcile report" className="mt-3 text-sm text-foreground/80">
          Reconcile done: {report.uploads_aborted} uploads aborted, {report.staging_deleted} staging files deleted, {report.orphan_blobs_deleted} orphan blobs deleted, {report.unreferenced_blobs_deleted} unreferenced blobs deleted
          {report.missing_blobs.length > 0 && <span className="text-destructive">; missing blobs: {report.missing_blobs.join(", ")}</span>}.
        </p>
      )}
    </Panel>
  );
}
