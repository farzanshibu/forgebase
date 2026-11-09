import { useState } from "react";
import { copyObject, deleteObject, downloadObject, listVersions, moveObject, objectMeta, restoreVersion } from "../../api";
import type { Bucket, ObjectInfo } from "../../api";
import { Button, ConfirmButton, ErrorView, Loading, Panel, inputCls } from "../../components/ui";
import { formatBytes } from "../../format";
import { useLoad } from "../../hooks";
import { when } from "../functions/util";
import SignedUrls from "./SignedUrls";
import { saveBlob } from "./util";

export default function ObjectPanel({ bucket, objectKey, onClose, onChanged }: { bucket: Bucket; objectKey: string; onClose: () => void; onChanged: () => void }) {
  const info = useLoad<ObjectInfo | undefined>((s) => objectMeta(bucket.name, objectKey, undefined, s), `obj:${bucket.name}:${objectKey}`);
  const versions = useLoad((s) => listVersions(bucket.name, objectKey, s), `versions:${bucket.name}:${objectKey}`);
  const [error, setError] = useState<unknown>();
  const [dest, setDest] = useState(objectKey);
  const [notice, setNotice] = useState("");
  const base = objectKey.split("/").pop() || objectKey;

  const act = async (f: () => Promise<unknown>, msg: string, closes = false) => {
    setError(undefined); setNotice("");
    try { await f(); setNotice(msg); onChanged(); info.reload(); versions.reload(); if (closes) onClose(); } catch (e) { setError(e); }
  };
  const download = async (version?: string) => {
    setError(undefined);
    try { saveBlob(await downloadObject(bucket.name, objectKey, version), base); } catch (e) { setError(e); }
  };
  const o = info.data;
  return (
    <aside aria-label="Object details" className="space-y-4">
      <Panel title={base} actions={<Button onClick={onClose} aria-label="Close details">Close</Button>}>
        <p className="break-all font-mono text-xs text-zinc-400">{objectKey}</p>
        {info.loading && !info.error && <Loading what="Loading object" />}
        {o && (
          <dl className="mt-2 grid grid-cols-[6rem_1fr] gap-y-1 text-sm">
            <dt className="text-zinc-500">Size</dt><dd>{formatBytes(o.size)}</dd>
            <dt className="text-zinc-500">Type</dt><dd>{o.content_type}</dd>
            <dt className="text-zinc-500">SHA-256</dt><dd className="break-all font-mono text-xs">{o.checksum_sha256 ?? "n/a"}</dd>
            <dt className="text-zinc-500">Version</dt><dd className="break-all font-mono text-xs">{o.version_id}</dd>
            <dt className="text-zinc-500">Created</dt><dd>{when(o.created_at)} by {o.created_by}</dd>
            {Object.entries(o.metadata).map(([k, v]) => (<><dt key={`k${k}`} className="text-zinc-500">meta {k}</dt><dd key={`v${k}`}>{v}</dd></>))}
          </dl>
        )}
        {info.data === undefined && !info.loading && !info.error && <p className="mt-2 text-sm text-zinc-400">This object no longer exists (or is a delete marker).</p>}
        <div className="mt-3 flex flex-wrap gap-2">
          <Button onClick={() => void download()}>Download</Button>
          <ConfirmButton label="Delete object" onConfirm={() => void act(() => deleteObject(bucket.name, objectKey), "Deleted.", !bucket.versioning)} />
        </div>
        {notice && <p role="status" className="mt-2 text-sm text-emerald-300">{notice}</p>}
        <div className="mt-2"><ErrorView error={error} /></div>
      </Panel>

      <Panel title="Copy or move">
        <label className="block text-sm">Destination key<input className={`${inputCls} mt-1 block w-full font-mono text-xs`} value={dest} onChange={(e) => setDest(e.target.value)} /></label>
        <div className="mt-2 flex gap-2">
          <Button disabled={!dest || dest === objectKey} onClick={() => void act(() => copyObject(bucket.name, objectKey, dest), `Copied to ${dest}.`)}>Copy</Button>
          <Button disabled={!dest || dest === objectKey} onClick={() => void act(() => moveObject(bucket.name, objectKey, dest), `Moved to ${dest}.`, true)}>Move</Button>
        </div>
      </Panel>

      <Panel title="Version history">
        <ErrorView error={versions.error} />
        {versions.loading && !versions.error && <Loading what="Loading versions" />}
        {versions.data && versions.data.length === 0 && <p className="text-sm text-zinc-400">No versions recorded.</p>}
        {versions.data && versions.data.length > 0 && (
          <ul aria-label="Versions" className="space-y-2 text-sm">
            {versions.data.map((v) => (
              <li key={v.version_id} className="rounded border border-zinc-800 p-2">
                <p className="break-all font-mono text-xs">{v.version_id}{v.is_latest && <span className="ml-2 rounded bg-emerald-900 px-1 text-emerald-200">latest</span>}{v.delete_marker && <span className="ml-2 rounded bg-red-900 px-1 text-red-200">delete marker</span>}</p>
                <p className="text-zinc-400">{v.delete_marker ? "no content" : formatBytes(v.size)} · {when(v.created_at)}</p>
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
        {!bucket.versioning && <p className="mt-2 text-xs text-zinc-500">Versioning is off for this bucket: overwrites and deletes do not keep history.</p>}
      </Panel>

      <SignedUrls bucket={bucket} initialKey={objectKey} />
    </aside>
  );
}
