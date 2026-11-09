import { useCallback, useEffect, useRef, useState } from "react";
import { useSearchParams } from "react-router-dom";
import { listObjects, uploadObject } from "../../api";
import type { Bucket, ObjectInfo } from "../../api";
import { Button, ErrorView, Loading, Panel, inputCls } from "../../components/ui";
import { formatBytes } from "../../format";
import { when } from "../functions/util";
import ObjectPanel from "./ObjectPanel";
import { baseName, crumbs } from "./util";

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
  const more = async () => {
    if (!list?.cursor) return;
    try {
      const p = await listObjects(bucket.name, { prefix, delimiter: "/", limit: PAGE, cursor: list.cursor });
      setRes({ id, scope, list: { objects: [...list.objects, ...p.objects], prefixes: [...new Set([...list.prefixes, ...p.prefixes])], cursor: p.next_cursor ?? null } });
    } catch (e) { setRes({ id, scope, list, error: e }); }
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
  const fileInput = useRef<HTMLInputElement>(null);

  return (
    <div className="grid gap-4 lg:grid-cols-[minmax(0,1fr)_22rem]">
      <div className="space-y-4">
        <Panel title="Objects" actions={<Button onClick={reload}>Refresh</Button>}>
          <nav aria-label="Folder path" className="mb-3 flex flex-wrap items-center gap-1 text-sm">
            <button type="button" className="text-amber-300 hover:underline" onClick={() => nav({ prefix: "" })}>{bucket.name}</button>
            {crumbs(prefix).map((c) => (
              <span key={c.prefix} className="flex items-center gap-1"><span className="text-zinc-600">/</span>
                <button type="button" className="text-amber-300 hover:underline" onClick={() => nav({ prefix: c.prefix })}>{c.label}</button>
              </span>
            ))}
          </nav>
          <ErrorView error={error} />
          {loading && !error && <Loading what="Loading objects" />}
          {list && list.objects.length === 0 && list.prefixes.length === 0 && <p className="text-sm text-zinc-400">This folder is empty.</p>}
          {list && (list.objects.length > 0 || list.prefixes.length > 0) && (
            <table aria-label="Objects" className="w-full text-left text-sm">
              <thead className="text-xs uppercase tracking-wide text-zinc-500"><tr><th className="py-1">Name</th><th>Size</th><th>Type</th><th>Modified</th></tr></thead>
              <tbody>
                {list.prefixes.map((p) => (
                  <tr key={p} className="border-t border-zinc-800">
                    <td className="py-1.5" colSpan={4}><button type="button" className="text-amber-300 hover:underline" aria-label={`Open folder ${baseName(p, prefix)}`} onClick={() => nav({ prefix: p })}>{baseName(p, prefix)}/</button></td>
                  </tr>
                ))}
                {list.objects.map((o) => (
                  <tr key={o.key} className={`border-t border-zinc-800 ${o.key === selected ? "bg-zinc-800/60" : ""}`}>
                    <td className="py-1.5"><button type="button" className="text-left text-zinc-100 hover:underline" aria-label={`Select ${baseName(o.key, prefix)}`} onClick={() => nav({ key: o.key })}>{baseName(o.key, prefix)}</button></td>
                    <td>{formatBytes(o.size)}</td><td className="text-zinc-400">{o.content_type}</td><td className="text-zinc-400">{when(o.created_at)}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
          {list?.cursor && <div className="mt-3"><Button onClick={() => void more()}>Load more</Button></div>}
        </Panel>

        <Panel title="Upload">
          <div className="flex flex-wrap items-center gap-3 text-sm">
            <input ref={fileInput} type="file" multiple aria-label="Choose files" onChange={(e) => { void upload(e.target.files); e.target.value = ""; }} />
            <span className="text-zinc-500">into {prefix || "the bucket root"}; files over 8 MiB upload in resumable chunks.</span>
          </div>
          <form className="mt-3 flex items-center gap-2 text-sm" onSubmit={(e) => { e.preventDefault(); const f = folder.trim().replace(/^\/+|\/+$/g, ""); if (f) { nav({ prefix: `${prefix}${f}/` }); setFolder(""); } }}>
            <input className={`${inputCls} w-44`} aria-label="New folder name" placeholder="new folder" value={folder} onChange={(e) => setFolder(e.target.value)} />
            <Button type="submit" disabled={!folder.trim()}>Open folder</Button>
          </form>
          {transfers.length > 0 && (
            <ul aria-label="Uploads" className="mt-3 space-y-2 text-sm">
              {transfers.map((t) => (
                <li key={t.id}>
                  <div className="flex justify-between"><span>{t.name}</span><span className="text-zinc-400">{t.error ? "failed" : t.done ? "done" : `${formatBytes(t.sent)} / ${formatBytes(t.total)}`}</span></div>
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
