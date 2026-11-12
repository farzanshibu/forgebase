import { useCallback, useEffect, useRef, useState } from "react";
import { useSearchParams } from "react-router-dom";
import { ArrowRight01Icon, BucketIcon, Cancel01Icon, CloudUploadIcon, Folder01Icon, FolderAddIcon, Refresh01Icon } from "@hugeicons/core-free-icons";
import { Input } from "panelui-web/input";
import { Table } from "panelui-web/table";
import { listObjects, uploadObject } from "../../api";
import type { Bucket, ObjectInfo } from "../../api";
import { EmptyState } from "../../components/EmptyState";
import { Pager } from "../../components/Pager";
import { SkeletonTable } from "../../components/Skeleton";
import { Button, ErrorView } from "../../components/ui";
import { formatBytes } from "../../format";
import { when } from "../functions/util";
import { Icon, IconButton } from "../database/parts";
import ObjectPanel from "./ObjectPanel";
import { baseName, crumbs, fileIcon } from "./util";

const PAGE = 100;
interface Listing { objects: ObjectInfo[]; prefixes: string[]; cursor: string | null }
interface Transfer { id: number; name: string; sent: number; total: number; error?: unknown; done?: boolean }

const crumbClass = "rounded px-1.5 py-0.5 hover:bg-muted hover:text-foreground focus-visible:outline-2 focus-visible:outline-ring";

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

  const [folderOpen, setFolderOpen] = useState(false);
  const [folder, setFolder] = useState("");
  const [over, setOver] = useState(false);
  const fileInput = useRef<HTMLInputElement>(null);
  const choose = () => fileInput.current?.click();
  const where = prefix ? `${bucket.name}/${prefix}` : bucket.name;
  const empty = !!list && list.objects.length === 0 && list.prefixes.length === 0;
  const finished = transfers.filter((t) => t.done || t.error !== undefined).length;

  return (
    <div className={`grid gap-4 ${selected ? "lg:grid-cols-[minmax(0,1fr)_20rem] xl:grid-cols-[minmax(0,1fr)_24rem]" : ""}`}>
      <section
        aria-label="Drop files here"
        onDragOver={(e) => { e.preventDefault(); setOver(true); }}
        onDragLeave={(e) => { if (!e.currentTarget.contains(e.relatedTarget as Node | null)) setOver(false); }}
        onDrop={(e) => { e.preventDefault(); setOver(false); void upload(e.dataTransfer.files); }}
        className="@container relative min-w-0 overflow-hidden rounded-xl border border-border bg-card"
      >
        <header className="flex flex-wrap items-center justify-between gap-x-4 gap-y-2 border-b border-border px-3 py-2.5">
          <nav aria-label="Folder path" className="flex min-w-0 flex-wrap items-center gap-0.5 text-sm">
            <button type="button" className={`inline-flex items-center gap-1.5 font-medium ${crumbClass} ${prefix ? "text-muted-foreground" : "text-foreground"}`} aria-current={prefix ? undefined : "location"} onClick={() => nav({ prefix: "" })}>
              <Icon icon={BucketIcon} size={14} />
              {bucket.name}
            </button>
            {crumbs(prefix).map((c, i, all) => (
              <span key={c.prefix} className="flex items-center gap-0.5">
                <Icon icon={ArrowRight01Icon} size={12} className="text-muted-foreground/60" />
                <button type="button" className={`${crumbClass} ${i === all.length - 1 ? "font-medium text-foreground" : "text-muted-foreground"}`} aria-current={i === all.length - 1 ? "location" : undefined} onClick={() => nav({ prefix: c.prefix })}>{c.label}</button>
              </span>
            ))}
          </nav>
          <div className="flex items-center gap-1.5">
            <IconButton icon={Refresh01Icon} label="Refresh" onClick={reload} className={loading && list ? "[&_svg]:animate-spin" : ""} />
            <Button onClick={() => setFolderOpen((o) => !o)} aria-expanded={folderOpen}>
              <Icon icon={FolderAddIcon} size={14} />
              New folder
            </Button>
            <Button tone="primary" onClick={choose}>
              <Icon icon={CloudUploadIcon} size={14} />
              Upload files
            </Button>
          </div>
        </header>
        <Input size="sm" ref={fileInput} type="file" multiple aria-label="Choose files" className="sr-only" tabIndex={-1} onChange={(e) => { void upload(e.target.files); e.target.value = ""; }} />

        {folderOpen && (
          <form
            className="flex flex-wrap items-center gap-2 border-b border-border bg-muted/30 px-3 py-2.5 text-sm"
            onSubmit={(e) => { e.preventDefault(); const f = folder.trim().replace(/^\/+|\/+$/g, ""); if (f) { nav({ prefix: `${prefix}${f}/` }); setFolder(""); setFolderOpen(false); } }}
          >
            <Icon icon={Folder01Icon} size={16} className="text-muted-foreground" />
            <Input size="sm" className="w-56" aria-label="New folder name" placeholder="folder name" value={folder} onChange={(e) => setFolder(e.target.value)} />
            <Button type="submit" disabled={!folder.trim()}>Open folder</Button>
            <IconButton icon={Cancel01Icon} label="Cancel new folder" onClick={() => { setFolderOpen(false); setFolder(""); }} />
            <span className="text-xs text-muted-foreground">Folders are part of file names: this one appears once you upload a file into it.</span>
          </form>
        )}

        {error !== undefined && <div className="p-4"><ErrorView error={error} onRetry={reload} /></div>}
        {loading && !error && !list && <div className="p-4"><SkeletonTable rows={4} columns={4} label="Loading objects" /></div>}
        {empty && (
          <div className="p-4">
            <EmptyState
              compact
              title="This folder is empty"
              why="Drop files anywhere on this panel, choose them with Upload files, or upload from your code."
              action={<Button tone="primary" onClick={choose}>Upload files</Button>}
            />
          </div>
        )}
        {list && !empty && (
          <Table framed density="compact" aria-label="Objects">
            <Table.Header className="bg-muted/40">
              <Table.Row>
                <Table.Head className="w-full">Name</Table.Head>
                <Table.Head align="end">Size</Table.Head>
                <Table.Head className="hidden @xl:table-cell">Type</Table.Head>
                <Table.Head align="end" className="hidden @md:table-cell">Modified</Table.Head>
              </Table.Row>
            </Table.Header>
            <Table.Body>
              {list.prefixes.map((p) => {
                const n = baseName(p, prefix);
                return (
                  <Table.Row key={p} className="cursor-pointer" onClick={(e) => { if (!(e.target as HTMLElement).closest("button")) nav({ prefix: p }); }}>
                    <Table.Cell className="py-2">
                      <button type="button" className="inline-flex items-center gap-2.5 rounded-sm font-medium text-foreground hover:text-primary focus-visible:outline-2 focus-visible:outline-ring" aria-label={`Open folder ${n}`} onClick={() => nav({ prefix: p })}>
                        <Icon icon={Folder01Icon} size={18} className="text-primary" />
                        {n}/
                      </button>
                    </Table.Cell>
                    <Table.Cell align="end" className="text-muted-foreground">–</Table.Cell>
                    <Table.Cell className="hidden text-muted-foreground @xl:table-cell">Folder</Table.Cell>
                    <Table.Cell align="end" className="hidden text-muted-foreground @md:table-cell">–</Table.Cell>
                  </Table.Row>
                );
              })}
              {list.objects.map((o) => {
                const n = baseName(o.key, prefix);
                const isSel = o.key === selected;
                return (
                  <Table.Row key={o.key} aria-selected={isSel || undefined} className="cursor-pointer" onClick={(e) => { if (!(e.target as HTMLElement).closest("button")) nav({ key: o.key }); }}>
                    <Table.Cell className="max-w-0 py-2">
                      <button type="button" className="inline-flex max-w-full items-center gap-2.5 rounded-sm text-left text-foreground hover:text-primary focus-visible:outline-2 focus-visible:outline-ring" aria-label={`Select ${n}`} title={n} onClick={() => nav({ key: o.key })}>
                        <Icon icon={fileIcon(o.key, o.content_type)} size={18} className="text-muted-foreground" />
                        <span className="truncate">{n}</span>
                      </button>
                    </Table.Cell>
                    <Table.Cell align="end" className="whitespace-nowrap">{formatBytes(o.size)}</Table.Cell>
                    <Table.Cell className="hidden max-w-[12rem] truncate text-xs text-muted-foreground @xl:table-cell" title={o.content_type}>{o.content_type}</Table.Cell>
                    <Table.Cell align="end" className="hidden whitespace-nowrap text-xs text-muted-foreground @md:table-cell"><span title={when(o.created_at)}>{when(o.created_at).slice(0, 16).replace("T", " ")}</span></Table.Cell>
                  </Table.Row>
                );
              })}
            </Table.Body>
          </Table>
        )}
        {list && !empty && (
          <div className="border-t border-border px-4 pb-3">
            <Pager mode="more" shown={list.objects.length + list.prefixes.length} hasMore={!!list.cursor} loading={loadingMore} onMore={() => void more()} noun="items" label="More objects" />
          </div>
        )}

        <section aria-label="Drop files here or choose files" className="m-3 flex flex-wrap items-center justify-center gap-x-2 gap-y-1 rounded-lg border border-dashed border-border px-4 py-3 text-center text-xs text-muted-foreground">
          <Icon icon={CloudUploadIcon} size={16} />
          <span>
            Drop files here or <button type="button" className="font-medium text-primary hover:underline focus-visible:outline-2 focus-visible:outline-ring" onClick={choose}>choose files</button> to upload into <span className="font-mono text-foreground">{where}</span>.
          </span>
          <span>Files over 8 MiB upload in resumable chunks, so a dropped connection resumes where it stopped.</span>
        </section>

        {transfers.length > 0 && (
          <div className="border-t border-border px-4 py-3">
            <div className="mb-2 flex items-center justify-between">
              <h3 className="text-xs font-medium text-muted-foreground">Uploads</h3>
              {finished > 0 && <button type="button" className="text-xs text-muted-foreground hover:text-foreground" onClick={() => setTransfers((ts) => ts.filter((t) => !t.done && t.error === undefined))}>Clear finished</button>}
            </div>
            <ul aria-label="Uploads" className="space-y-2.5 text-sm">
              {transfers.map((t) => (
                <li key={t.id}>
                  <div className="flex items-center gap-2">
                    <Icon icon={fileIcon(t.name)} size={15} className="text-muted-foreground" />
                    <span className="min-w-0 flex-1 truncate">{t.name}</span>
                    <span className={`text-xs tabular-nums ${t.error ? "text-destructive" : t.done ? "text-success" : "text-muted-foreground"}`}>{t.error ? "failed" : t.done ? "done" : `${formatBytes(t.sent)} / ${formatBytes(t.total)}`}</span>
                  </div>
                  <progress
                    aria-label={`Upload ${t.name}`}
                    className={`mt-1.5 block h-1 w-full appearance-none overflow-hidden rounded-full bg-muted [&::-webkit-progress-bar]:bg-muted [&::-webkit-progress-value]:rounded-full ${t.error !== undefined ? "[&::-moz-progress-bar]:bg-destructive [&::-webkit-progress-value]:bg-destructive" : t.done ? "[&::-moz-progress-bar]:bg-success [&::-webkit-progress-value]:bg-success" : "[&::-moz-progress-bar]:bg-primary [&::-webkit-progress-value]:bg-primary"}`}
                    value={t.sent}
                    max={Math.max(t.total, 1)}
                  />
                  {t.error !== undefined && <div className="mt-1.5"><ErrorView error={t.error} /></div>}
                </li>
              ))}
            </ul>
          </div>
        )}

        {over && (
          <div aria-hidden="true" className="pointer-events-none absolute inset-0 z-10 flex flex-col items-center justify-center gap-2 rounded-xl border-2 border-dashed border-primary bg-primary/5 text-sm font-medium text-primary backdrop-blur-[1px]">
            <Icon icon={CloudUploadIcon} size={28} />
            Drop to upload into {where}
          </div>
        )}
      </section>
      {selected && <ObjectPanel key={`${selected}#${uploaded}`} bucket={bucket} objectKey={selected} onClose={() => nav({ key: null })} onChanged={reload} />}
    </div>
  );
}
