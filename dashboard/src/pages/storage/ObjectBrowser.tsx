import { useCallback, useEffect, useRef, useState } from "react";
import { useSearchParams } from "react-router-dom";
import { listObjects, uploadObject } from "../../api";
import type { Bucket, ObjectInfo } from "../../api";
import { EmptyState } from "../../components/EmptyState";
import { Pager } from "../../components/Pager";
import { SkeletonTable } from "../../components/Skeleton";
import { Button, ErrorView, Panel } from "../../components/ui";
import { formatBytes } from "../../format";
import { when } from "../functions/util";
import ObjectPanel from "./ObjectPanel";
import { baseName, crumbs } from "./util";
import { Table } from "panelui-web/table";
import { Input } from "panelui-web/input";

const PAGE = 100;
interface Listing { objects: ObjectInfo[]; prefixes: string[]; cursor: string | null }
interface Transfer { id: number; name: string; sent: number; total: number; error?: unknown; done?: boolean }

export default function ObjectBrowser({ bucket }: { bucket: Bucket }) {
  const [params, setParams] = useSearchParams();
  const prefix = params.get("prefix") ?? "";
  const selected = params.get("key");
  const nav = (next: { prefix?: string; key?: string | null }) => {
    const p = new URLSearchParams(params);
    if (next.prefix !== undefined) { if (next.prefix) p.set("prefix", next.prefix); else p.delete("prefix"); p.delete("key"); }
    if (next.key !== undefined) { if (next.key) p.set("key", next.key); else p.delete("key"); }
    setParams(p);
  };

  const [res, setRes] = useState<{ id: string; scope: string; list?: Listing; error?: unknown }>();
  const [tick, setTick] = useState(0);
  const reload = useCallback(() => setTick((t) => t + 1), []);
  const scope = JSON.stringify([bucket.name, prefix]);
  const id = `${scope}#${tick}`;
  const loading = res?.id !== id;
  // After a refresh keep showing the previous listing of the same folder rather than flashing empty.
  const list = res?.scope === scope ? res.list : undefined;
  const error = res?.id === id ? res.error : undefined;
  useEffect(() => {
    const ctrl = new AbortController();
    listObjects(bucket.name, { prefix, delimiter: "/", limit: PAGE }, ctrl.signal).then(
      (p) => { if (!ctrl.signal.aborted) setRes({ id, scope, list: { objects: p.objects, prefixes: p.prefixes, cursor: p.next_cursor ?? null } }); },
      (e: unknown) => { if (!ctrl.signal.aborted) setRes({ id, scope, error: e }); },
    );
    return () => ctrl.abort();
  }, [bucket.name, prefix, id, scope]);
  const [loadingMore, setLoadingMore] = useState(false);
  const more = async () => {
    if (!list?.cursor) return;
    setLoadingMore(true);
    try {
      const p = await listObjects(bucket.name, { prefix, delimiter: "/", limit: PAGE, cursor: list.cursor });
      setRes({ id, scope, list: { objects: [...list.objects, ...p.objects], prefixes: [...new Set([...list.prefixes, ...p.prefixes])], cursor: p.next_cursor ?? null } });
    } catch (e) { setRes({ id, scope, list, error: e }); } finally { setLoadingMore(false); }
  };

  // Uploads.
  const [transfers, setTransfers] = useState<Transfer[]>([]);
  const [uploaded, setUploaded] = useState(0);
  const seq = useRef(0);
  const upload = async (files: FileList | null) => {
    if (!files) return;
    for (const f of Array.from(files)) {
      const id = ++seq.current;
      const patch = (p: Partial<Transfer>) => setTransfers((ts) => ts.map((t) => (t.id === id ? { ...t, ...p } : t)));
      setTransfers((ts) => [...ts, { id, name: f.name, sent: 0, total: f.size }]);
      try {
        await uploadObject(bucket.name, prefix + f.name, f, { onProgress: (sent, total) => patch({ sent, total }) });
        patch({ done: true });
        setUploaded((n) => n + 1); // a selected object's panel (versions) is stale now
        reload();
      } catch (e) { patch({ error: e }); }
    }
  };

  const [folder, setFolder] = useState("");
  const [over, setOver] = useState(false);
  const fileInput = useRef<HTMLInputElement>(null);

  return (
    <div className="grid gap-4 lg:grid-cols-[minmax(0,1fr)_22rem]">
      <div className="space-y-4">
        <Panel title="Objects" actions={<Button onClick={reload}>Refresh</Button>}>
          <nav aria-label="Folder path" className="mb-3 flex flex-wrap items-center gap-1 text-sm">
            <button type="button" className="text-primary hover:underline" onClick={() => nav({ prefix: "" })}>{bucket.name}</button>
            {crumbs(prefix).map((c) => (
              <span key={c.prefix} className="flex items-center gap-1"><span className="text-muted-foreground">/</span>
                <button type="button" className="text-primary hover:underline" onClick={() => nav({ prefix: c.prefix })}>{c.label}</button>
              </span>
            ))}
          </nav>
          <ErrorView error={error} onRetry={reload} />
          {loading && !error && !list && <SkeletonTable rows={4} columns={4} label="Loading objects" />}
          {list && list.objects.length === 0 && list.prefixes.length === 0 && <EmptyState compact title="This folder is empty" why="Drop files into the upload area below, or upload from your code." />}
          {list && (list.objects.length > 0 || list.prefixes.length > 0) && (
            <Table aria-label="Objects" className="w-full text-left text-sm">
              <Table.Header className="text-xs uppercase tracking-wide text-muted-foreground"><Table.Row><Table.Head className="py-1">Name</Table.Head><Table.Head>Size</Table.Head><Table.Head>Type</Table.Head><Table.Head>Modified</Table.Head></Table.Row></Table.Header>
              <Table.Body>
                {list.prefixes.map((p) => (
                  <Table.Row key={p} className="border-t border-border">
                    <Table.Cell className="py-1.5" colSpan={4}><button type="button" className="text-primary hover:underline" aria-label={`Open folder ${baseName(p, prefix)}`} onClick={() => nav({ prefix: p })}>{baseName(p, prefix)}/</button></Table.Cell>
                  </Table.Row>
                ))}
                {list.objects.map((o) => (
                  <Table.Row key={o.key} className={`border-t border-border ${o.key === selected ? "bg-muted/60" : ""}`}>
                    <Table.Cell className="py-1.5"><button type="button" className="text-left text-foreground hover:underline" aria-label={`Select ${baseName(o.key, prefix)}`} onClick={() => nav({ key: o.key })}>{baseName(o.key, prefix)}</button></Table.Cell>
                    <Table.Cell>{formatBytes(o.size)}</Table.Cell><Table.Cell className="text-muted-foreground">{o.content_type}</Table.Cell><Table.Cell className="text-muted-foreground">{when(o.created_at)}</Table.Cell>
                  </Table.Row>
                ))}
              </Table.Body>
            </Table>
          )}
          {list && (list.objects.length > 0 || list.prefixes.length > 0) && <Pager mode="more" shown={list.objects.length + list.prefixes.length} hasMore={!!list.cursor} loading={loadingMore} onMore={() => void more()} noun="items" label="More objects" />}
        </Panel>

        <Panel title="Upload">
          <div
            aria-label="Drop files here"
            onDragOver={(e) => { e.preventDefault(); setOver(true); }}
            onDragLeave={() => setOver(false)}
            onDrop={(e) => { e.preventDefault(); setOver(false); void upload(e.dataTransfer.files); }}
            className={over ? "rounded-xl ring-2 ring-primary" : undefined}
          >
            <EmptyState
              compact
              title="Drop files here or choose files"
              why={`They go into ${prefix || "the bucket root"}. Files over 8 MiB upload in resumable chunks, so a dropped connection resumes where it stopped.`}
              action={<Button onClick={() => fileInput.current?.click()}>Choose files</Button>}
            />
            <Input size="sm" ref={fileInput} type="file" multiple aria-label="Choose files" className="sr-only" tabIndex={-1} onChange={(e) => { void upload(e.target.files); e.target.value = ""; }} />
          </div>
          <form className="mt-3 flex items-center gap-2 text-sm" onSubmit={(e) => { e.preventDefault(); const f = folder.trim().replace(/^\/+|\/+$/g, ""); if (f) { nav({ prefix: `${prefix}${f}/` }); setFolder(""); } }}>
            <Input size="sm" className="w-44" aria-label="New folder name" placeholder="new folder" value={folder} onChange={(e) => setFolder(e.target.value)} />
            <Button type="submit" disabled={!folder.trim()}>Open folder</Button>
          </form>
          {transfers.length > 0 && (
            <ul aria-label="Uploads" className="mt-3 space-y-2 text-sm">
              {transfers.map((t) => (
                <li key={t.id}>
                  <div className="flex justify-between"><span>{t.name}</span><span className="text-muted-foreground">{t.error ? "failed" : t.done ? "done" : `${formatBytes(t.sent)} / ${formatBytes(t.total)}`}</span></div>
                  <progress aria-label={`Upload ${t.name}`} className="h-1.5 w-full" value={t.sent} max={Math.max(t.total, 1)} />
                  {t.error !== undefined && <ErrorView error={t.error} />}
                </li>
              ))}
            </ul>
          )}
        </Panel>
      </div>
      {selected && <ObjectPanel key={`${selected}#${uploaded}`} bucket={bucket} objectKey={selected} onClose={() => nav({ key: null })} onChanged={reload} />}
    </div>
  );
}
