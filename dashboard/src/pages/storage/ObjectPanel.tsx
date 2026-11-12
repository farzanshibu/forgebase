import { useState, type ReactNode } from "react";
import { Cancel01Icon, Copy01Icon, Delete02Icon, Download04Icon } from "@hugeicons/core-free-icons";
import { Badge } from "panelui-web/badge";
import { Input } from "panelui-web/input";
import { copyObject, deleteObject, downloadObject, listVersions, moveObject, objectMeta, restoreVersion } from "../../api";
import type { Bucket, ObjectInfo } from "../../api";
import { ConfirmDialog } from "../../components/ConfirmDialog";
import { EmptyState } from "../../components/EmptyState";
import { SkeletonLines } from "../../components/Skeleton";
import { UseFromCode } from "../../components/UseFromCode";
import { Button, ErrorView } from "../../components/ui";
import { notify, toaster } from "../../toaster";
import { formatBytes } from "../../format";
import { useLoad } from "../../hooks";
import { when } from "../functions/util";
import { Icon, IconButton } from "../database/parts";
import SignedUrls from "./SignedUrls";
import { fileIcon, saveBlob } from "./util";

/** A titled card of the details column. */
function Card({ title, children, actions }: { title: ReactNode; children: ReactNode; actions?: ReactNode }) {
  return (
    <section className="rounded-xl border border-border bg-card">
      <header className="flex items-center justify-between gap-2 border-b border-border px-4 py-2.5">
        <h3 className="text-sm font-medium">{title}</h3>
        {actions}
      </header>
      <div className="p-4">{children}</div>
    </section>
  );
}

/** A long id: start and end shown, the whole value in the tooltip and behind a copy button. */
function LongId({ value, label }: { value: string; label: string }) {
  const short = value.length > 20 ? `${value.slice(0, 10)}…${value.slice(-8)}` : value;
  return (
    <span className="flex min-w-0 items-center gap-1">
      <span className="truncate font-mono text-xs" title={value}>{short}</span>
      <IconButton
        icon={Copy01Icon}
        label={`Copy ${label}`}
        className="size-6 shrink-0 [&_svg]:size-3.5"
        onClick={() => void navigator.clipboard?.writeText(value).then(() => notify.copied(label), () => notify.failed(new Error("The browser refused to copy. Select the text instead.")))}
      />
    </span>
  );
}

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
    <aside aria-label="Object details" className="min-w-0 space-y-4">
      <section className="rounded-xl border border-border bg-card">
        <header className="flex items-start gap-3 border-b border-border px-4 py-3">
          <span className="flex size-9 shrink-0 items-center justify-center rounded-lg bg-muted text-muted-foreground"><Icon icon={fileIcon(objectKey, o?.content_type)} size={18} /></span>
          <div className="min-w-0 flex-1">
            <h2 className="truncate text-sm font-medium" title={base}>{base}</h2>
            <p className="truncate font-mono text-xs text-muted-foreground" title={objectKey}>{objectKey}</p>
          </div>
          <IconButton icon={Cancel01Icon} label="Close details" onClick={onClose} />
        </header>
        <div className="p-4">
          {info.loading && !info.error && <SkeletonLines lines={4} label="Loading object" />}
          <ErrorView error={info.error} onRetry={info.reload} />
          {o && (
            <dl className="grid grid-cols-[5.5rem_minmax(0,1fr)] gap-x-3 gap-y-2 text-sm">
              <dt className="text-muted-foreground">Size</dt><dd className="tabular-nums">{formatBytes(o.size)}</dd>
              <dt className="text-muted-foreground">Type</dt><dd className="truncate" title={o.content_type}>{o.content_type}</dd>
              <dt className="text-muted-foreground">Created</dt><dd><span className="tabular-nums">{when(o.created_at)}</span><span className="block truncate text-xs text-muted-foreground" title={o.created_by}>by {o.created_by}</span></dd>
              <dt className="text-muted-foreground">SHA-256</dt><dd>{o.checksum_sha256 ? <LongId value={o.checksum_sha256} label="SHA-256" /> : <span className="text-muted-foreground">n/a</span>}</dd>
              <dt className="text-muted-foreground">Version</dt><dd><LongId value={o.version_id} label="version id" /></dd>
              {Object.entries(o.metadata).map(([k, v]) => (
                <div key={k} className="contents">
                  <dt className="truncate text-muted-foreground" title={k}>{k}</dt><dd className="break-all">{v}</dd>
                </div>
              ))}
            </dl>
          )}
          {info.data === undefined && !info.loading && !info.error && <EmptyState compact title="This object is gone" why="It no longer exists, or its latest version is a delete marker." />}
          <div className="mt-4 flex flex-wrap gap-2">
            <Button tone="primary" onClick={() => void download()}>
              <Icon icon={Download04Icon} size={14} />
              Download
            </Button>
            <ConfirmDialog
              trigger={<Button tone="danger"><Icon icon={Delete02Icon} size={14} />Delete object</Button>}
              title={`Delete ${base}?`}
              consequence={bucket.versioning ? "The object gets a delete marker; earlier versions stay and can be restored." : "The object is deleted for good. Apps that read it get 404."}
              confirmLabel="Delete object"
              onConfirm={async () => { await deleteObject(bucket.name, objectKey); notify.deleted(`Object ${base}`); onChanged(); info.reload(); versions.reload(); if (!bucket.versioning) onClose(); }}
            />
          </div>
          {error !== undefined && <div className="mt-3"><ErrorView error={error} /></div>}
        </div>
      </section>

      <Card title="Copy or move">
        <label className="block text-sm">
          <span className="text-muted-foreground">Destination key</span>
          <Input size="sm" className="mt-1 block w-full font-mono text-xs" value={dest} onChange={(e) => setDest(e.target.value)} />
        </label>
        <div className="mt-3 flex gap-2">
          <Button disabled={!dest || dest === objectKey} onClick={() => void act(() => copyObject(bucket.name, objectKey, dest), `Copied to ${dest}.`)}>Copy</Button>
          <Button disabled={!dest || dest === objectKey} onClick={() => void act(() => moveObject(bucket.name, objectKey, dest), `Moved to ${dest}.`, true)}>Move</Button>
        </div>
      </Card>

      <Card title="Version history" actions={versions.data && versions.data.length > 0 ? <span className="text-xs tabular-nums text-muted-foreground">{versions.data.length}</span> : undefined}>
        <ErrorView error={versions.error} />
        {versions.loading && !versions.error && <SkeletonLines lines={2} label="Loading versions" />}
        {versions.data && versions.data.length === 0 && <EmptyState compact title="No versions recorded" why="Turn on versioning in the bucket policy to keep earlier versions of this file." />}
        {versions.data && versions.data.length > 0 && (
          <ol aria-label="Versions" className="relative space-y-3 border-l border-border pl-4 text-sm">
            {versions.data.map((v) => (
              <li key={v.version_id} className="relative">
                <span aria-hidden="true" className={`absolute -left-[1.3rem] top-1.5 size-2 rounded-full ring-2 ring-card ${v.is_latest ? "bg-success" : v.delete_marker ? "bg-destructive" : "bg-muted-foreground/50"}`} />
                <p className="flex flex-wrap items-center gap-1.5">
                  <span className="truncate font-mono text-xs" title={v.version_id}>{v.version_id.length > 16 ? `${v.version_id.slice(0, 16)}…` : v.version_id}</span>
                  {v.is_latest && <Badge variant="success" size="sm">latest</Badge>}
                  {v.delete_marker && <Badge variant="destructive" size="sm">delete marker</Badge>}
                </p>
                <p className="text-xs tabular-nums text-muted-foreground">{v.delete_marker ? "no content" : formatBytes(v.size)} · {when(v.created_at)}</p>
                {!v.delete_marker && (
                  <div className="mt-1.5 flex gap-2">
                    <Button className="h-7 px-2 text-xs" aria-label={`Download version ${v.version_id}`} onClick={() => void download(v.version_id)}>Download</Button>
                    {!v.is_latest && <Button className="h-7 px-2 text-xs" aria-label={`Restore version ${v.version_id}`} onClick={() => void act(() => restoreVersion(bucket.name, objectKey, v.version_id), "Version restored as the current version.")}>Restore</Button>}
                  </div>
                )}
              </li>
            ))}
          </ol>
        )}
        {!bucket.versioning && <p className="mt-3 text-xs text-muted-foreground">Versioning is off for this bucket: overwrites and deletes do not keep history.</p>}
      </Card>

      <SignedUrls bucket={bucket} initialKey={objectKey} compact />

      <Card title="Use from code">
        <UseFromCode kind="object" name={`${bucket.name}/${objectKey}`} title={null} />
      </Card>
    </aside>
  );
}
