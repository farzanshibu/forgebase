import { useState } from "react";
import { Link } from "react-router-dom";
import { ApiError, VISIBILITIES, bucketUsage, createBucket, deleteBucket, listBuckets, reconcileStorage, storageQuotaBytes, storageStats } from "../../api";
import type { ReconcileReport, Visibility } from "../../api";
import { Button, ConfirmButton, ErrorView, Loading, Panel, inputCls } from "../../components/ui";
import { formatBytes } from "../../format";
import { useLoad } from "../../hooks";
import { when } from "../functions/util";
import { parseBytes } from "./util";

export default function BucketList() {
  const buckets = useLoad(listBuckets, "buckets");
  const [error, setError] = useState<unknown>();
  const [force, setForce] = useState(false);
  const act = async (f: () => Promise<unknown>) => {
    setError(undefined);
    try { await f(); buckets.reload(); } catch (e) { setError(e); }
  };
  return (
    <div className="space-y-4">
      <Usage />
      <ErrorView error={error} />
      <Panel title="Buckets" actions={<label className="flex items-center gap-2 text-xs text-zinc-400"><input type="checkbox" checked={force} onChange={(e) => setForce(e.target.checked)} />Delete with their objects</label>}>
        <ErrorView error={buckets.error} />
        {buckets.loading && !buckets.error && <Loading what="Loading buckets" />}
        {buckets.data && buckets.data.length === 0 && <p className="text-sm text-zinc-400">No buckets yet. Create one below.</p>}
        {buckets.data && buckets.data.length > 0 && (
          <table aria-label="Buckets" className="w-full text-left text-sm">
            <thead className="text-xs uppercase tracking-wide text-zinc-500"><tr><th className="py-1">Name</th><th>Visibility</th><th>Versioning</th><th>Max object</th><th>Used</th><th>Created</th><th /></tr></thead>
            <tbody>
              {buckets.data.map((b) => (
                <tr key={b.name} className="border-t border-zinc-800">
                  <td className="py-2"><Link className="text-amber-300 hover:underline" to={`/storage/b/${encodeURIComponent(b.name)}`}>{b.name}</Link></td>
                  <td>{b.visibility}</td><td>{b.versioning ? "on" : "off"}</td>
                  <td>{b.max_object_bytes ? formatBytes(b.max_object_bytes) : "no limit"}</td>
                  <td><BucketUsageCell name={b.name} /></td>
                  <td className="text-zinc-400">{when(b.created_at)}</td>
                  <td className="text-right"><ConfirmButton label={`Delete bucket ${b.name}`} onConfirm={() => void act(() => deleteBucket(b.name, force))} /></td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </Panel>
      <NewBucket onCreated={buckets.reload} />
    </div>
  );
}

/** Objects and bytes of one bucket from `GET .../buckets/{bucket}/usage`; a refusal shows "n/a". */
function BucketUsageCell({ name }: { name: string }) {
  const u = useLoad((s) => bucketUsage(name, s), `bucket-usage:${name}`);
  if (u.error) return <span className="text-zinc-500">n/a</span>;
  if (!u.data) return <span className="text-zinc-500">…</span>;
  return <span aria-label={`Usage of ${name}`}>{formatBytes(u.data.logical_bytes)} · {u.data.objects} object{u.data.objects === 1 ? "" : "s"}</span>;
}

function NewBucket({ onCreated }: { onCreated: () => void }) {
  const [name, setName] = useState("");
  const [visibility, setVisibility] = useState<Visibility>("private");
  const [versioning, setVersioning] = useState(false);
  const [max, setMax] = useState("");
  const [error, setError] = useState<unknown>();
  const [busy, setBusy] = useState(false);
  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    setError(undefined);
    setBusy(true);
    try {
      await createBucket({ name: name.trim(), visibility, versioning, max_object_bytes: parseBytes(max) });
      setName(""); setMax("");
      onCreated();
    } catch (err) { setError(err); } finally { setBusy(false); }
  };
  return (
    <Panel title="New bucket">
      <form onSubmit={(e) => void submit(e)} className="flex flex-wrap items-end gap-3 text-sm">
        <label>Name<input className={`${inputCls} ml-2 w-44`} value={name} onChange={(e) => setName(e.target.value)} required /></label>
        <label>Visibility
          <select className={`${inputCls} ml-2`} value={visibility} onChange={(e) => setVisibility(e.target.value as Visibility)}>
            {VISIBILITIES.map((v) => <option key={v}>{v}</option>)}
          </select>
        </label>
        <label className="flex items-center gap-2"><input type="checkbox" checked={versioning} onChange={(e) => setVersioning(e.target.checked)} />Versioning</label>
        <label>Max object size<input className={`${inputCls} ml-2 w-28`} placeholder="no limit" value={max} onChange={(e) => setMax(e.target.value)} /></label>
        <Button type="submit" tone="primary" disabled={busy || !name.trim()}>Create bucket</Button>
      </form>
      {error !== undefined && <div className="mt-3"><ErrorView error={error} /></div>}
    </Panel>
  );
}

/** Instance usage against the storage.bytes entitlement, plus the reconciler. Both need admin rights; a refusal is shown, not fatal. */
function Usage() {
  const stats = useLoad(storageStats, "storage-stats");
  const quota = useLoad((s) => storageQuotaBytes(s).catch(() => null), "storage-quota");
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
  return (
    <Panel title="Usage" actions={<Button onClick={() => void run()} disabled={busy}>Run reconcile</Button>}>
      {stats.error instanceof ApiError && stats.error.isAuth ? <p className="text-sm text-zinc-400">Instance totals need the storage_admin permission.</p> : <ErrorView error={stats.error} />}
      {stats.data && (
        <div className="space-y-2 text-sm">
          <p aria-label="Storage totals" className="text-zinc-300">
            {stats.data.buckets} buckets · {stats.data.objects} objects · {stats.data.versions} versions · {formatBytes(stats.data.logical_bytes)} logical · {formatBytes(stats.data.stored_bytes)} stored · {stats.data.open_uploads} open uploads
          </p>
          {limit !== null ? (
            <div>
              <p aria-label="Quota usage" className="text-zinc-400">{formatBytes(used)} of {formatBytes(limit)} used ({pct}%)</p>
              <div role="progressbar" aria-label="Storage quota" aria-valuenow={pct} aria-valuemin={0} aria-valuemax={100} className="mt-1 h-2 w-full overflow-hidden rounded bg-zinc-800">
                <div className={`h-full ${pct >= 90 ? "bg-red-500" : pct >= 70 ? "bg-amber-400" : "bg-emerald-500"}`} style={{ width: `${pct}%` }} />
              </div>
            </div>
          ) : <p className="text-zinc-500">No storage quota is reported for this account.</p>}
        </div>
      )}
      <ErrorView error={error} />
      {report && (
        <p aria-label="Reconcile report" className="mt-3 text-sm text-zinc-300">
          Reconcile done: {report.uploads_aborted} uploads aborted, {report.staging_deleted} staging files deleted, {report.orphan_blobs_deleted} orphan blobs deleted, {report.unreferenced_blobs_deleted} unreferenced blobs deleted
          {report.missing_blobs.length > 0 && <span className="text-red-300">; missing blobs: {report.missing_blobs.join(", ")}</span>}.
        </p>
      )}
    </Panel>
  );
}
