import { useState } from "react";
import { copyObject, deleteObject, downloadObject, listVersions, moveObject, objectMeta, restoreVersion } from "../../api";
import type { Bucket, ObjectInfo } from "../../api";
import { ConfirmDialog } from "../../components/ConfirmDialog";
import { EmptyState } from "../../components/EmptyState";
import { SkeletonLines } from "../../components/Skeleton";
import { UseFromCode } from "../../components/UseFromCode";
import { Button, ErrorView, Panel } from "../../components/ui";
import { notify, toaster } from "../../toaster";
import { formatBytes } from "../../format";
import { useLoad } from "../../hooks";
import { when } from "../functions/util";
import SignedUrls from "./SignedUrls";
import { saveBlob } from "./util";
import { Input } from "panelui-web/input";

export default function ObjectPanel({ bucket, objectKey, onClose, onChanged }: { bucket: Bucket; objectKey: string; onClose: () => void; onChanged: () => void }) {
  const info = useLoad<ObjectInfo | undefined>((s) => objectMeta(bucket.name, objectKey, undefined, s), `obj:${bucket.name}:${objectKey}`);
  const versions = useLoad((s) => listVersions(bucket.name, objectKey, s), `versions:${bucket.name}:${objectKey}`);
  const [error, setError] = useState<unknown>();
  const [dest, setDest] = useState(objectKey);
  const base = objectKey.split("/").pop() || objectKey;

  const act = async (f: () => Promise<unknown>, msg: string, closes = false) => {
    setError(undefined);
    try { await f(); toaster.add({ title: msg.replace(/\.$/, ""), type: "success" }); onChanged(); info.reload(); versions.reload(); if (closes) onClose(); } catch (e) { setError(e); }
  };
  const download = async (version?: string) => {
    setError(undefined);
    try { saveBlob(await downloadObject(bucket.name, objectKey, version), base); } catch (e) { setError(e); }
  };
  const o = info.data;
  return (
    <aside aria-label="Object details" className="space-y-4">
      <Panel title={base} actions={<Button onClick={onClose} aria-label="Close details">Close</Button>}>
        <p className="break-all font-mono text-xs text-muted-foreground">{objectKey}</p>
        {info.loading && !info.error && <SkeletonLines lines={4} label="Loading object" />}
        {o && (
          <dl className="mt-2 grid grid-cols-[6rem_1fr] gap-y-1 text-sm">
            <dt className="text-muted-foreground">Size</dt><dd>{formatBytes(o.size)}</dd>
            <dt className="text-muted-foreground">Type</dt><dd>{o.content_type}</dd>
            <dt className="text-muted-foreground">SHA-256</dt><dd className="break-all font-mono text-xs">{o.checksum_sha256 ?? "n/a"}</dd>
            <dt className="text-muted-foreground">Version</dt><dd className="break-all font-mono text-xs">{o.version_id}</dd>
            <dt className="text-muted-foreground">Created</dt><dd>{when(o.created_at)} by {o.created_by}</dd>
            {Object.entries(o.metadata).map(([k, v]) => (<><dt key={`k${k}`} className="text-muted-foreground">meta {k}</dt><dd key={`v${k}`}>{v}</dd></>))}
          </dl>
        )}
        {info.data === undefined && !info.loading && !info.error && <EmptyState compact className="mt-2" title="This object is gone" why="It no longer exists, or its latest version is a delete marker." />}
        <div className="mt-3 flex flex-wrap gap-2">
          <Button onClick={() => void download()}>Download</Button>
          <ConfirmDialog
            trigger={<Button tone="danger">Delete object</Button>}
            title={`Delete ${base}?`}
            consequence={bucket.versioning ? "The object gets a delete marker; earlier versions stay and can be restored." : "The object is deleted for good. Apps that read it get 404."}
            confirmLabel="Delete object"
            onConfirm={async () => { await deleteObject(bucket.name, objectKey); notify.deleted(`Object ${base}`); onChanged(); info.reload(); versions.reload(); if (!bucket.versioning) onClose(); }}
          />
        </div>
        <div className="mt-2"><ErrorView error={error} /></div>
      </Panel>

      <Panel title="Copy or move">
        <label className="block text-sm">Destination key<Input size="sm" className="mt-1 block w-full font-mono text-xs" value={dest} onChange={(e) => setDest(e.target.value)} /></label>
        <div className="mt-2 flex gap-2">
          <Button disabled={!dest || dest === objectKey} onClick={() => void act(() => copyObject(bucket.name, objectKey, dest), `Copied to ${dest}.`)}>Copy</Button>
          <Button disabled={!dest || dest === objectKey} onClick={() => void act(() => moveObject(bucket.name, objectKey, dest), `Moved to ${dest}.`, true)}>Move</Button>
        </div>
      </Panel>

      <Panel title="Version history">
        <ErrorView error={versions.error} />
        {versions.loading && !versions.error && <SkeletonLines lines={2} label="Loading versions" />}
        {versions.data && versions.data.length === 0 && <EmptyState compact title="No versions recorded" why="Turn on versioning in the bucket policy to keep earlier versions of this file." />}
        {versions.data && versions.data.length > 0 && (
          <ul aria-label="Versions" className="space-y-2 text-sm">
            {versions.data.map((v) => (
              <li key={v.version_id} className="rounded border border-border p-2">
                <p className="break-all font-mono text-xs">{v.version_id}{v.is_latest && <span className="ml-2 rounded bg-success/15 px-1 text-success">latest</span>}{v.delete_marker && <span className="ml-2 rounded bg-destructive/15 px-1 text-destructive">delete marker</span>}</p>
                <p className="text-muted-foreground">{v.delete_marker ? "no content" : formatBytes(v.size)} · {when(v.created_at)}</p>
                {!v.delete_marker && (
                  <div className="mt-1 flex gap-2">
                    <Button aria-label={`Download version ${v.version_id}`} onClick={() => void download(v.version_id)}>Download</Button>
                    {!v.is_latest && <Button aria-label={`Restore version ${v.version_id}`} onClick={() => void act(() => restoreVersion(bucket.name, objectKey, v.version_id), "Version restored as the current version.")}>Restore</Button>}
                  </div>
                )}
              </li>
            ))}
          </ul>
        )}
        {!bucket.versioning && <p className="mt-2 text-xs text-muted-foreground">Versioning is off for this bucket: overwrites and deletes do not keep history.</p>}
      </Panel>

      <SignedUrls bucket={bucket} initialKey={objectKey} />

      <Panel title="Use from code">
        <UseFromCode kind="object" name={`${bucket.name}/${objectKey}`} title={null} />
      </Panel>
    </aside>
  );
}
