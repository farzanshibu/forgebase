import { useCallback, useEffect, useState } from "react";
import { useSearchParams } from "react-router-dom";
import { KV_TYPES, kvFlush, kvScan, kvSetString } from "../../api";
import type { KvType } from "../../api";
import { ConfirmDialog } from "../../components/ConfirmDialog";
import { CreateSheet } from "../../components/CreateSheet";
import { EmptyState } from "../../components/EmptyState";
import { FormField } from "../../components/FormField";
import { Pager } from "../../components/Pager";
import { SkeletonLines } from "../../components/Skeleton";
import { Button, ErrorView } from "../../components/ui";
import { notify } from "../../toaster";
import { validate } from "../../validate";
import { binFrom } from "../../api";
import KeyViewer from "./KeyViewer";
import { Input } from "panelui-web/input";
import { Sel } from "../../components/controls";

const PAGE = 50;
/** The Type filter's "any" option (a select cannot show an empty value). */
const ANY = "any";

export default function KeyBrowser({ ns, creating, setCreating }: { ns: string; creating: boolean; setCreating: (open: boolean) => void }) {
  const [params, setParams] = useSearchParams();
  const selected = params.get("key");
  const select = (k: string | null) => { const p = new URLSearchParams(params); if (k) p.set("key", k); else p.delete("key"); setParams(p); };

  const [pattern, setPattern] = useState("");
  const [type, setType] = useState<KvType | "">("");
  const [applied, setApplied] = useState({ pattern: "", type: "" as KvType | "" });
  const [res, setRes] = useState<{ id: string; filter: string; keys: string[]; cursor: string; error?: unknown }>();
  const [tick, setTick] = useState(0);
  const filter = JSON.stringify([ns, applied.pattern, applied.type]);
  const id = `${filter}#${tick}`;
  const fresh = res?.id === id;
  const loading = !fresh;
  // After a refresh keep showing the previous page of the same filter rather than flashing empty.
  const keys = res?.filter === filter ? res.keys : [];
  const cursor = fresh ? res.cursor : "0";
  const error = fresh ? res.error : undefined;
  const reload = useCallback(() => setTick((t) => t + 1), []);

  useEffect(() => {
    const ctrl = new AbortController();
    kvScan(ns, { match: applied.pattern || undefined, type: applied.type, count: PAGE }, ctrl.signal).then(
      (r) => { if (!ctrl.signal.aborted) setRes({ id, filter, keys: r.keys, cursor: r.cursor }); },
      (e: unknown) => { if (!ctrl.signal.aborted) setRes({ id, filter, keys: [], cursor: "0", error: e }); },
    );
    return () => ctrl.abort();
  }, [ns, applied.pattern, applied.type, id, filter]);

  const more = async () => {
    try {
      const r = await kvScan(ns, { cursor, match: applied.pattern || undefined, type: applied.type, count: PAGE });
      setRes({ id, filter, keys: [...new Set([...keys, ...r.keys])], cursor: r.cursor });
    } catch (e) { setRes({ id, filter, keys, cursor, error: e }); }
  };
  const flush = async () => {
    const r = await kvFlush(ns);
    notify.deleted(`${r.deleted} keys of ${ns}`);
    select(null);
    reload();
  };

  const filtered = !!(applied.pattern || applied.type);
  return (
    <div className="grid items-start gap-4 lg:grid-cols-[minmax(0,20rem)_minmax(0,1fr)]">
      <div className="space-y-3">
        <section aria-labelledby="kv-keys" className="overflow-hidden rounded-xl border border-border bg-card">
          <header className="flex items-center justify-between gap-2 border-b border-border px-3 py-2.5">
            <h2 id="kv-keys" className="text-sm font-medium">Keys{keys.length > 0 && <span className="ml-2 text-xs font-normal tabular-nums text-muted-foreground">{keys.length}{cursor !== "0" ? "+" : ""}</span>}</h2>
            <Button onClick={reload} disabled={loading}>{loading && keys.length > 0 ? "Refreshing…" : "Refresh"}</Button>
          </header>
          <form className="space-y-1.5 border-b border-border px-3 py-3 text-sm" onSubmit={(e) => { e.preventDefault(); setApplied({ pattern: pattern.trim(), type }); }}>
            <div className="flex items-end gap-2">
              <FormField label="Pattern" className="min-w-0 flex-1">{(p) => <Input {...p} size="sm" className="w-full font-mono" placeholder="user:*" value={pattern} onChange={(e) => setPattern(e.target.value)} aria-describedby="kv-pattern-help" />}</FormField>
              <FormField label="Type">{(p) => <Sel {...p} aria-label="Type" className="w-24" value={type || ANY} onChange={(v) => setType(v === ANY ? "" : (v as KvType))} options={[{ value: ANY, label: "any" }, ...KV_TYPES.map((t) => ({ value: t, label: t }))]} />}</FormField>
            </div>
            <div className="flex items-center justify-between gap-2">
              <p id="kv-pattern-help" className="text-xs text-muted-foreground"><code className="font-mono">*</code> matches any text, <code className="font-mono">?</code> one character.</p>
              <Button type="submit">Apply filter</Button>
            </div>
          </form>
          <div className="p-2">
            <ErrorView error={error} />
            {loading && !error && keys.length === 0 && <div className="p-2"><SkeletonLines lines={4} label="Scanning keys" /></div>}
            {!loading && !error && keys.length === 0 && (filtered
              ? <EmptyState compact title="No keys match" why="Change the pattern or the type, or clear them to see every key." />
              : <EmptyState compact title={`No keys in ${ns} yet`} why="A key holds one value: text, a hash, a list, a set, a sorted set or a stream. Caches, counters and sessions live here." action={<Button tone="primary" onClick={() => setCreating(true)}>New key</Button>} docs="docs/adr/0005-kv.md" />)}
            {keys.length > 0 && (
              <ul aria-label="Keys" className="max-h-[28rem] space-y-px overflow-auto text-sm">
                {keys.map((k) => (
                  <li key={k}>
                    <button type="button" title={k} aria-current={k === selected ? "true" : undefined} onClick={() => select(k)}
                      className={`block w-full truncate rounded-md border-l-2 px-2.5 py-1.5 text-left font-mono text-xs transition-colors focus-visible:outline-2 focus-visible:outline-ring ${k === selected ? "border-primary bg-accent font-medium text-foreground" : "border-transparent text-foreground/90 hover:bg-muted"}`}>{k}</button>
                  </li>
                ))}
              </ul>
            )}
            {keys.length > 0 && <div className="px-2"><Pager mode="more" shown={keys.length} hasMore={cursor !== "0"} onMore={() => void more()} noun="keys" label="More keys" /></div>}
          </div>
        </section>
        <div className="flex flex-wrap items-center justify-between gap-2 rounded-xl border border-destructive/30 px-3 py-2.5">
          <p className="text-xs text-muted-foreground">Delete every key in <span className="font-mono text-foreground">{ns}</span>.</p>
          <ConfirmDialog
            trigger={<Button tone="danger">Flush namespace {ns}</Button>}
            title={`Flush namespace ${ns}?`}
            consequence="Every key in this namespace is deleted at once. Apps that read them get nothing back. This cannot be undone."
            confirmLabel="Flush namespace"
            confirmText={ns}
            onConfirm={flush}
          />
        </div>
      </div>
      {selected ? <KeyViewer key={`${ns}/${selected}`} ns={ns} keyName={selected} onGone={() => { select(null); reload(); }} onChanged={reload} /> : <EmptyState title="Select a key" why="Choose a key on the left to see its value, TTL and type, and to edit them." className="min-h-64 justify-center" />}
      <NewKeySheet ns={ns} open={creating} onOpenChange={setCreating} onCreated={reload} />
    </div>
  );
}

function NewKeySheet({ ns, open, onOpenChange, onCreated }: { ns: string; open: boolean; onOpenChange: (o: boolean) => void; onCreated: () => void }) {
  const [key, setKey] = useState("");
  const [value, setValue] = useState("");
  const [ttl, setTtl] = useState("");
  const ttlError = ttl.trim() ? validate.integer(1)(ttl) : undefined;
  return (
    <CreateSheet
      open={open}
      onOpenChange={onOpenChange}
      title="New key"
      description={`Adds a text key to namespace ${ns}. Other types are created from the Console or from your code.`}
      submitLabel="Create key"
      canSubmit={!!key && !ttlError}
      onSubmit={async () => {
        await kvSetString(ns, key, binFrom(value), ttl.trim() ? { ttl_ms: Number(ttl) * 1000 } : {});
        const k = key;
        setKey(""); setValue(""); setTtl("");
        onCreated();
        // Navigating after the sheet closes keeps `?key=` (two URL updates in one tick would drop one).
        return { what: `Key ${k}`, href: `/redis?${new URLSearchParams({ ...(ns !== "default" ? { ns } : {}), key: k })}` };
      }}
    >
      <FormField label="Key" required>{(p) => <Input {...p} size="sm" className="font-mono" placeholder="user:42:session" value={key} onChange={(e) => setKey(e.target.value)} />}</FormField>
      <FormField label="Value">{(p) => <Input {...p} size="sm" value={value} onChange={(e) => setValue(e.target.value)} />}</FormField>
      <FormField label="Expire after (seconds)" term="ttl" error={ttlError} help="Leave empty to keep the key until you delete it.">{(p) => <Input {...p} size="sm" inputMode="numeric" value={ttl} onChange={(e) => setTtl(e.target.value)} />}</FormField>
    </CreateSheet>
  );
}
