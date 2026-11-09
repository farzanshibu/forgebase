import { useCallback, useEffect, useState } from "react";
import { useSearchParams } from "react-router-dom";
import { KV_TYPES, kvFlush, kvScan, kvSetString } from "../../api";
import type { KvType } from "../../api";
import { Button, ConfirmButton, ErrorView, Loading, Panel, inputCls } from "../../components/ui";
import KeyViewer from "./KeyViewer";

const PAGE = 50;

export default function KeyBrowser({ ns }: { ns: string }) {
  const [params, setParams] = useSearchParams();
  const selected = params.get("key");
  const select = (k: string | null) => { const p = new URLSearchParams(params); if (k) p.set("key", k); else p.delete("key"); setParams(p); };

  const [pattern, setPattern] = useState("");
  const [type, setType] = useState<KvType | "">("");
  const [applied, setApplied] = useState({ pattern: "", type: "" as KvType | "" });
  const [keys, setKeys] = useState<string[]>([]);
  const [cursor, setCursor] = useState("0");
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<unknown>();
  const [tick, setTick] = useState(0);
  const reload = useCallback(() => setTick((t) => t + 1), []);

  useEffect(() => {
    const ctrl = new AbortController();
    setLoading(true); setError(undefined);
    kvScan(ns, { match: applied.pattern || undefined, type: applied.type, count: PAGE }, ctrl.signal).then(
      (r) => { if (!ctrl.signal.aborted) { setKeys(r.keys); setCursor(r.cursor); setLoading(false); } },
      (e: unknown) => { if (!ctrl.signal.aborted) { setError(e); setLoading(false); } },
    );
    return () => ctrl.abort();
  }, [ns, applied, tick]);

  const more = async () => {
    try {
      const r = await kvScan(ns, { cursor, match: applied.pattern || undefined, type: applied.type, count: PAGE });
      setKeys((k) => [...new Set([...k, ...r.keys])]);
      setCursor(r.cursor);
    } catch (e) { setError(e); }
  };
  const [newKey, setNewKey] = useState("");
  const [newVal, setNewVal] = useState("");
  const [createError, setCreateError] = useState<unknown>();
  const create = async (e: React.FormEvent) => {
    e.preventDefault();
    setCreateError(undefined);
    try { await kvSetString(ns, newKey, newVal); const k = newKey; setNewKey(""); setNewVal(""); reload(); select(k); } catch (err) { setCreateError(err); }
  };
  const flush = async () => { try { await kvFlush(ns); select(null); reload(); } catch (e) { setError(e); } };

  return (
    <div className="grid gap-4 lg:grid-cols-[minmax(0,22rem)_minmax(0,1fr)]">
      <div className="space-y-4">
        <Panel title="Keys" actions={<Button onClick={reload}>Refresh</Button>}>
          <form className="mb-3 flex flex-wrap items-end gap-2 text-sm" onSubmit={(e) => { e.preventDefault(); setApplied({ pattern: pattern.trim(), type }); }}>
            <label>Pattern<input className={`${inputCls} ml-2 w-32`} placeholder="user:*" value={pattern} onChange={(e) => setPattern(e.target.value)} /></label>
            <label>Type
              <select className={`${inputCls} ml-2`} value={type} onChange={(e) => setType(e.target.value as KvType | "")}>
                <option value="">any</option>{KV_TYPES.map((t) => <option key={t}>{t}</option>)}
              </select>
            </label>
            <Button type="submit">Apply filter</Button>
          </form>
          <ErrorView error={error} />
          {loading && !error && <Loading what="Scanning keys" />}
          {!loading && !error && keys.length === 0 && <p className="text-sm text-zinc-400">No keys match in namespace {ns}.</p>}
          {keys.length > 0 && (
            <ul aria-label="Keys" className="max-h-[28rem] overflow-auto text-sm">
              {keys.map((k) => (
                <li key={k}><button type="button" onClick={() => select(k)} className={`block w-full truncate rounded px-2 py-1 text-left font-mono text-xs hover:bg-zinc-800 ${k === selected ? "bg-zinc-800 text-amber-300" : "text-zinc-200"}`}>{k}</button></li>
              ))}
            </ul>
          )}
          {cursor !== "0" && <div className="mt-3"><Button onClick={() => void more()}>Load more keys</Button></div>}
          <p className="mt-2 text-xs text-zinc-500">{keys.length} key{keys.length === 1 ? "" : "s"} shown{cursor !== "0" ? " (more available)" : ""}.</p>
        </Panel>
        <Panel title="New string key">
          <form onSubmit={(e) => void create(e)} className="space-y-2 text-sm">
            <input aria-label="New key name" className={`${inputCls} w-full font-mono`} placeholder="key" value={newKey} onChange={(e) => setNewKey(e.target.value)} />
            <input aria-label="New key value" className={`${inputCls} w-full`} placeholder="value" value={newVal} onChange={(e) => setNewVal(e.target.value)} />
            <Button type="submit" tone="primary" disabled={!newKey}>Create key</Button>
          </form>
          <div className="mt-2"><ErrorView error={createError} /></div>
          <div className="mt-3"><ConfirmButton label={`Flush namespace ${ns}`} onConfirm={() => void flush()} /></div>
        </Panel>
      </div>
      {selected ? <KeyViewer key={`${ns}/${selected}`} ns={ns} keyName={selected} onGone={() => { select(null); reload(); }} onChanged={reload} /> : <Panel><p className="text-sm text-zinc-400">Select a key to view and edit it.</p></Panel>}
    </div>
  );
}
