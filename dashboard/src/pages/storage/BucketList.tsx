import { useState, type ReactNode } from "react";
import { Link, useNavigate } from "react-router-dom";
import { BucketIcon, Delete02Icon, PlusSignIcon, Search01Icon } from "@hugeicons/core-free-icons";
import { Badge } from "panelui-web/badge";
import { Input } from "panelui-web/input";
import { Table } from "panelui-web/table";
import { ApiError, VISIBILITIES, bucketUsage, createBucket, deleteBucket, listBuckets, reconcileStorage, rotateSigningKey, storageQuotaFor, storageStats } from "../../api";
import type { Bucket, ReconcileReport, StorageStats, Visibility } from "../../api";
import { useSession } from "../../auth/session";
import { Check, Sel } from "../../components/controls";
import { ConfirmDialog } from "../../components/ConfirmDialog";
import { CreateSheet } from "../../components/CreateSheet";
import { EmptyState } from "../../components/EmptyState";
import { fieldErrors } from "../../components/ErrorPanel";
import { FormField } from "../../components/FormField";
import { HelpTip } from "../../components/HelpTip";
import { PageHeader } from "../../components/PageHeader";
import { SkeletonCards, SkeletonTable } from "../../components/Skeleton";
import { Button, ErrorView } from "../../components/ui";
import { formatBytes } from "../../format";
import { useCreateParam, useLoad, type LoadState } from "../../hooks";
import { notify } from "../../toaster";
import { validate } from "../../validate";
import { when } from "../functions/util";
import { Icon, IconButton, TableCard } from "../database/parts";
import { VisibilityBadge } from "./parts";
import { VISIBILITY_LOOK, bucketScope, parseBytes } from "./util";

/** "2026-11-12" with the full time in the tooltip. */
const day = (iso: string) => <span title={when(iso)}>{when(iso).slice(0, 10)}</span>;

export default function BucketList() {
  const buckets = useLoad(listBuckets, "buckets");
  const stats = useLoad(storageStats, "storage-stats");
  const [creating, setCreating] = useCreateParam();
  const [q, setQ] = useState("");
  const newBucket = (
    <Button tone="primary" onClick={() => setCreating(true)}>
      <Icon icon={PlusSignIcon} size={14} />
      New bucket
    </Button>
  );
  const shown = (buckets.data ?? []).filter((b) => b.name.toLowerCase().includes(q.trim().toLowerCase()));
  return (
    <div className="space-y-6">
      <PageHeader studio="storage" primary={newBucket} />
      <Totals stats={stats} />
      <ErrorView error={buckets.error} onRetry={buckets.reload} />
      {buckets.loading && !buckets.error && <SkeletonTable rows={3} columns={6} label="Loading buckets" />}
      {buckets.data && buckets.data.length === 0 && (
        <EmptyState
          title="No buckets yet"
          why="Buckets hold the files your app uploads, like avatars or exports. Create one, then drop files into it or upload from your code."
          action={newBucket}
          icon={BucketIcon}
          docs="docs/adr/0018-object-storage.md"
        />
      )}
      {buckets.data && buckets.data.length > 0 && (
        <TableCard
          title="Buckets"
          count={buckets.data.length}
          actions={
            <label className="relative block">
              <span className="pointer-events-none absolute inset-y-0 left-2.5 flex items-center text-muted-foreground"><Icon icon={Search01Icon} size={14} /></span>
              <Input size="sm" type="search" aria-label="Find a bucket" placeholder="Find a bucket" className="w-56 pl-8" value={q} onChange={(e) => setQ(e.target.value)} />
            </label>
          }
        >
          {shown.length === 0 ? (
            <div className="p-4"><EmptyState compact title="No buckets match" why="Clear the search to see every bucket." /></div>
          ) : (
            <Table framed density="compact" aria-label="Buckets">
              <Table.Header className="bg-muted/40">
                <Table.Row>
                  <Table.Head>Name</Table.Head>
                  <Table.Head>Visibility</Table.Head>
                  <Table.Head>Versioning</Table.Head>
                  <Table.Head>Scope</Table.Head>
                  <Table.Head align="end">Objects</Table.Head>
                  <Table.Head align="end">Size</Table.Head>
                  <Table.Head align="end" className="hidden xl:table-cell">Max object</Table.Head>
                  <Table.Head align="end" className="hidden xl:table-cell">Created</Table.Head>
                  <Table.Head align="end"><span className="sr-only">Actions</span></Table.Head>
                </Table.Row>
              </Table.Header>
              <Table.Body>
                {shown.map((b) => <BucketRow key={b.name} b={b} onDeleted={buckets.reload} />)}
              </Table.Body>
            </Table>
          )}
        </TableCard>
      )}
      <Maintenance onReconciled={stats.reload} />
      <NewBucketSheet open={creating} onOpenChange={setCreating} onCreated={buckets.reload} />
    </div>
  );
}

/** One bucket: its own usage call fills Objects and Size; a refusal shows "n/a". The whole row opens the bucket. */
function BucketRow({ b, onDeleted }: { b: Bucket; onDeleted: () => void }) {
  const nav = useNavigate();
  const href = `/storage/b/${encodeURIComponent(b.name)}`;
  const u = useLoad((s) => bucketUsage(b.name, s), `bucket-usage:${b.name}`);
  const scope = bucketScope(b);
  const usage = (v: (x: NonNullable<typeof u.data>) => ReactNode, label: string) =>
    u.error ? <span className="text-muted-foreground">n/a</span> : !u.data ? <span className="text-muted-foreground">…</span> : <span aria-label={label}>{v(u.data)}</span>;
  return (
    <Table.Row
      className="cursor-pointer"
      onClick={(e) => {
        if ((e.target as HTMLElement).closest("a, button, [role=dialog], [role=alertdialog]")) return;
        nav(href);
      }}
    >
      <Table.Cell className="py-2.5">
        <span className="flex items-center gap-2.5">
          <span className="flex size-7 shrink-0 items-center justify-center rounded-md bg-muted text-muted-foreground"><Icon icon={BucketIcon} size={15} /></span>
          <Link className="font-medium text-foreground hover:text-primary hover:underline" to={href}>{b.name}</Link>
        </span>
      </Table.Cell>
      <Table.Cell><VisibilityBadge value={b.visibility} /></Table.Cell>
      <Table.Cell>{b.versioning ? <Badge variant="success" size="sm" dot>on</Badge> : <span className="text-muted-foreground">off</span>}</Table.Cell>
      <Table.Cell title={scope.text} className={b.tenant ? "" : "text-muted-foreground"}>{scope.label}</Table.Cell>
      <Table.Cell align="end">{usage((x) => x.objects, `Objects in ${b.name}`)}</Table.Cell>
      <Table.Cell align="end" className="whitespace-nowrap">{usage((x) => formatBytes(x.logical_bytes), `Size of ${b.name}`)}</Table.Cell>
      <Table.Cell align="end" className="hidden whitespace-nowrap text-muted-foreground xl:table-cell">{b.max_object_bytes ? formatBytes(b.max_object_bytes) : "no limit"}</Table.Cell>
      <Table.Cell align="end" className="hidden whitespace-nowrap text-muted-foreground xl:table-cell">{day(b.created_at)}</Table.Cell>
      <Table.Cell align="end"><DeleteBucket name={b.name} onDeleted={onDeleted} /></Table.Cell>
    </Table.Row>
  );
}

/** Delete one bucket. "Also delete its objects" is part of this bucket's dialog, not a page-wide switch (A-13). */
function DeleteBucket({ name, onDeleted }: { name: string; onDeleted: () => void }) {
  const [alsoObjects, setAlsoObjects] = useState(false);
  return (
    <ConfirmDialog
      trigger={<IconButton icon={Delete02Icon} tone="danger" label={`Delete bucket ${name}`} />}
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
        {(p) => <Input {...p} size="sm" className="font-mono" placeholder="avatars" value={name} onChange={(e) => { setName(e.target.value); setTouched(true); setSaveError(undefined); }} />}
      </FormField>
      <FormField label="Visibility" help={VISIBILITY_LOOK[visibility].text + "."}>
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

function Stat({ label, value, sub }: { label: string; value: ReactNode; sub?: ReactNode }) {
  return (
    <div className="rounded-xl border border-border bg-card px-4 py-3">
      <dt className="text-xs text-muted-foreground">{label}</dt>
      <dd className="mt-1 text-xl font-semibold tabular-nums tracking-tight">{value}</dd>
      {sub && <dd className="mt-0.5 text-xs text-muted-foreground">{sub}</dd>}
    </div>
  );
}

/**
 * Instance totals and usage against the `storage.bytes` entitlement. These need administrator rights; a refusal is
 * shown, not fatal. Entitlements belong to an organization or a signed-in user, so an administrator's session asks
 * for none (A-21).
 */
function Totals({ stats }: { stats: LoadState<StorageStats> }) {
  const session = useSession();
  const subject = session.admin && !session.impersonation ? null : "caller";
  const quota = useLoad((s) => storageQuotaFor(subject, s).catch(() => null), `storage-quota:${subject ?? "none"}`);
  const used = stats.data?.logical_bytes ?? 0;
  const limit = quota.data ?? null;
  const pct = limit ? Math.min(100, Math.round((used / limit) * 100)) : 0;
  const d = stats.data;
  if (stats.error instanceof ApiError && stats.error.isAuth) return <p className="text-sm text-muted-foreground">Instance totals need the storage_admin permission.</p>;
  if (stats.error) return <ErrorView error={stats.error} onRetry={stats.reload} />;
  if (!d) return <SkeletonCards count={4} label="Loading storage totals" />;
  return (
    <div className="space-y-3">
      {d.scope === "tenant" && <Badge variant="info" size="sm">Tenant {d.tenant ?? ""} only</Badge>}
      <dl aria-label="Storage totals" className="grid grid-cols-2 gap-3 sm:grid-cols-4">
        <Stat label="Buckets" value={d.buckets} />
        <Stat label="Objects" value={d.objects} sub={`${d.versions} versions`} />
        <Stat label="Size" value={formatBytes(d.logical_bytes)} sub={`${formatBytes(d.stored_bytes)} stored`} />
        <Stat label="Open uploads" value={d.open_uploads} sub="resumable sessions in progress" />
      </dl>
      {limit !== null ? (
        <div className="rounded-xl border border-border bg-card px-4 py-3">
          <p aria-label="Quota usage" className="flex justify-between text-sm"><span className="font-medium">Quota</span><span className="tabular-nums text-muted-foreground">{formatBytes(used)} of {formatBytes(limit)} used ({pct}%)</span></p>
          <div role="progressbar" aria-label="Storage quota" aria-valuenow={pct} aria-valuemin={0} aria-valuemax={100} className="mt-2 h-2 w-full overflow-hidden rounded-full bg-muted">
            <div className={`h-full rounded-full ${pct >= 90 ? "bg-destructive" : pct >= 70 ? "bg-warning" : "bg-success"}`} style={{ width: `${pct}%` }} />
          </div>
        </div>
      ) : (
        <p className="text-xs text-muted-foreground">
          {subject === null ? "No quota applies to administrators. " : "No quota set for this account. "}
          A quota belongs to an organization or user: set it in <Link className="text-primary hover:underline" to="/billing">Billing</Link>.
        </p>
      )}
    </div>
  );
}

/** Rare, instance-wide jobs: the reconciler and the signing key. Kept below the list, out of the way of everyday work. */
function Maintenance({ onReconciled }: { onReconciled: () => void }) {
  const [report, setReport] = useState<ReconcileReport>();
  const [error, setError] = useState<unknown>();
  const [busy, setBusy] = useState(false);
  const run = async () => {
    setBusy(true); setError(undefined);
    try { setReport(await reconcileStorage()); onReconciled(); } catch (e) { setError(e); } finally { setBusy(false); }
  };
  return (
    <section aria-label="Maintenance" className="rounded-xl border border-border bg-card">
      <h2 className="border-b border-border px-4 py-3 text-sm font-medium">Maintenance</h2>
      <div className="divide-y divide-border">
        <div className="flex flex-wrap items-center justify-between gap-3 px-4 py-3">
          <div className="min-w-0">
            <p className="flex items-center gap-1.5 text-sm font-medium">Reconcile storage <HelpTip term="reconcile" /></p>
            <p className="text-xs text-muted-foreground">Ends abandoned uploads and removes files on disk that no object points to.</p>
          </div>
          <Button onClick={() => void run()} disabled={busy}>{busy ? "Reconciling…" : "Run reconcile"}</Button>
        </div>
        <div className="flex flex-wrap items-center justify-between gap-3 px-4 py-3">
          <div className="min-w-0">
            <p className="text-sm font-medium">Signing key</p>
            <p className="text-xs text-muted-foreground">Rotating it makes every signed URL issued so far stop working.</p>
          </div>
          <ConfirmDialog
            trigger={<Button tone="danger">Rotate signing key</Button>}
            title="Rotate the signing key?"
            consequence="Every signed URL issued so far stops working at once, including the ones apps already handed out. New URLs work as usual."
            confirmLabel="Rotate key"
            onConfirm={async () => { await rotateSigningKey(); notify.saved("Signing key"); }}
          />
        </div>
      </div>
      {(error !== undefined || report) && (
        <div className="border-t border-border px-4 py-3">
          <ErrorView error={error} />
          {report && (
            <p aria-label="Reconcile report" className="text-sm text-foreground/80">
              Reconcile done: {report.uploads_aborted} uploads aborted, {report.staging_deleted} staging files deleted, {report.orphan_blobs_deleted} orphan blobs deleted, {report.unreferenced_blobs_deleted} unreferenced blobs deleted
              {report.missing_blobs.length > 0 && <span className="text-destructive">; missing blobs: {report.missing_blobs.join(", ")}</span>}.
            </p>
          )}
        </div>
      )}
    </section>
  );
}
