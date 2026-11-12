import { Fragment, useState } from "react";
import {
  ApiError, binFrom, binText, kvClearTtl, kvDelete, kvGet, kvHashDelete, kvHashSet, kvListPop, kvListPush, kvListRemove, kvListSet, kvSetAdd, kvSetRemove, kvSetString, kvSetTtl,
  kvStreamAdd, kvStreamDelete, kvZAdd, kvZRemove,
} from "../../api";
import type { Bin, KvKey, StreamEntry, ZMember } from "../../api";
import { ConfirmDialog } from "../../components/ConfirmDialog";
import { EmptyState } from "../../components/EmptyState";
import { HelpTip } from "../../components/HelpTip";
import { SkeletonLines } from "../../components/Skeleton";
import { Badge } from "panelui-web/badge";
import { Button, CopyButton, ErrorView } from "../../components/ui";
import { notify } from "../../toaster";
import { formatBytes } from "../../format";
import { useLoad } from "../../hooks";
import { TTL_UNITS, formatTtl, omit, parseFields } from "./util";
import { Table } from "panelui-web/table";
import { Input } from "panelui-web/input";
import { Textarea } from "panelui-web/textarea";
import { Sel } from "../../components/controls";

type Act = (f: () => Promise<unknown>) => Promise<void>;
interface EditorProps { ns: string; data: KvKey; act: Act }

const SIZE_NOUN: Record<string, string> = { hash: "fields", stream: "entries", list: "items", set: "members", zset: "members" };

export default function KeyViewer({ ns, keyName, onGone, onChanged }: { ns: string; keyName: string; onGone: () => void; onChanged: () => void }) {
  const kv = useLoad((s) => kvGet(ns, keyName, 1000, s), `kv:${ns}:${keyName}`);
  const [error, setError] = useState<unknown>();
  const act: Act = async (f) => {
    setError(undefined);
    try { await f(); kv.reload(); onChanged(); } catch (e) { setError(e); }
  };
  if (kv.error instanceof ApiError && kv.error.status === 404) return <EmptyState compact title={`Key ${keyName} is gone`} why="It was deleted or it expired." action={<Button onClick={onGone}>Back to keys</Button>} />;
  const d = kv.data;
  return (
    <div className="min-w-0 space-y-4" aria-label="Key details">
      <section aria-labelledby="kv-key-name" className="overflow-hidden rounded-xl border border-border bg-card">
        <header className="flex flex-wrap items-center justify-between gap-3 border-b border-border px-4 py-3">
          <div className="flex min-w-0 items-center gap-2">
            {d && <Badge variant="info" size="sm" className="font-mono">{d.type}</Badge>}
            <h2 id="kv-key-name" className="truncate font-mono text-sm font-medium" title={keyName}>{keyName}</h2>
          </div>
          <div className="flex shrink-0 items-center gap-2">
            <CopyButton text={keyName} label="Copy key" />
            {d && (
              <ConfirmDialog
                trigger={<Button tone="danger">Delete key</Button>}
                title={`Delete key ${keyName}?`}
                consequence="The key and its value are removed. Apps that read it get nothing back."
                confirmLabel="Delete key"
                onConfirm={async () => { await kvDelete(ns, keyName); notify.deleted(`Key ${keyName}`); onGone(); }}
              />
            )}
          </div>
        </header>
        <div className="space-y-3 px-4 py-3">
          <ErrorView error={kv.error} />
          {kv.loading && !kv.error && <SkeletonLines lines={3} label="Loading key" />}
          {d && (
            <>
              <dl aria-label="Key summary" className="grid grid-cols-2 gap-x-6 gap-y-2 text-sm sm:grid-cols-3">
                <div><dt className="text-xs text-muted-foreground">Type</dt><dd className="font-mono">{d.type}</dd></div>
                <div><dt className="text-xs text-muted-foreground">Size</dt><dd className="tabular-nums">{d.type === "string" ? formatBytes(d.size) : `${d.size} ${SIZE_NOUN[d.type] ?? "members"}`}{d.truncated && <span className="ml-1 text-xs text-warning">(first 1000 shown)</span>}</dd></div>
                <div><dt className="flex items-center gap-1 text-xs text-muted-foreground"><HelpTip term="ttl">TTL</HelpTip></dt><dd><span aria-label="Remaining TTL" className={`tabular-nums ${d.ttl_ms === null || d.ttl_ms === undefined ? "text-muted-foreground" : ""}`}>{formatTtl(d.ttl_ms)}</span></dd></div>
              </dl>
              <Ttl ns={ns} data={d} act={act} />
            </>
          )}
          <ErrorView error={error} />
        </div>
      </section>
      {d && (
        <section aria-labelledby="kv-key-value" className="overflow-hidden rounded-xl border border-border bg-card">
          <h3 id="kv-key-value" className="border-b border-border px-4 py-2.5 text-sm font-medium">Value</h3>
          <div className="p-4">
            {d.type === "string" && <StringEditor ns={ns} data={d} act={act} />}
            {d.type === "hash" && <HashEditor ns={ns} data={d} act={act} />}
            {d.type === "list" && <ListEditor ns={ns} data={d} act={act} />}
            {d.type === "set" && <SetEditor ns={ns} data={d} act={act} />}
            {d.type === "zset" && <ZSetEditor ns={ns} data={d} act={act} />}
            {d.type === "stream" && <StreamEditor ns={ns} data={d} act={act} />}
          </div>
        </section>
      )}
    </div>
  );
}

function Ttl({ ns, data, act }: EditorProps) {
  const [amount, setAmount] = useState("60");
  const [unit, setUnit] = useState(1000);
  const ms = Math.round(Number(amount) * unit);
  return (
    <div className="flex flex-wrap items-center gap-2 rounded-lg bg-muted/40 px-3 py-2 text-sm">
      <span className="text-muted-foreground">Expire in</span>
      <Input size="sm" aria-label="TTL amount" type="number" min="1" className="w-20 tabular-nums" value={amount} onChange={(e) => setAmount(e.target.value)} />
      <Sel aria-label="TTL unit" className="w-28" value={unit} onChange={setUnit} options={TTL_UNITS.map((u) => ({ value: u.ms, label: u.label }))} />
      <Button disabled={!(ms > 0)} onClick={() => void act(() => kvSetTtl(ns, data.key, ms))}>Set TTL</Button>
      <Button disabled={data.ttl_ms === null || data.ttl_ms === undefined} onClick={() => void act(() => kvClearTtl(ns, data.key))}>Remove TTL</Button>
    </div>
  );
}

function StringEditor({ ns, data, act }: EditorProps) {
  const initial = binText(data.value as Bin);
  const [text, setText] = useState(initial);
  return (
    <div className="space-y-2">
      <Textarea size="sm" autoResize={false} aria-label="String value" className="block h-40 w-full font-mono text-xs" value={text} onChange={(e) => setText(e.target.value)} />
      <p className="text-xs text-muted-foreground">Binary values show as <span className="font-mono">base64:…</span>; keep the prefix to store bytes. Saving keeps the TTL.</p>
      <Button tone="primary" disabled={text === initial} onClick={() => void act(() => kvSetString(ns, data.key, binFrom(text), { keepttl: true }))}>Save value</Button>
    </div>
  );
}

function HashEditor({ ns, data, act }: EditorProps) {
  const fields = Object.entries(data.value as Record<string, Bin>);
  const [edits, setEdits] = useState<Record<string, string>>({});
  const [name, setName] = useState("");
  const [val, setVal] = useState("");
  return (
    <div className="space-y-3">
      {fields.length === 0 && <EmptyState compact title="No fields" why="Add the first field below." />}
      {fields.length > 0 && (
        <Table aria-label="Hash fields" density="compact" className="w-full text-left text-sm">
          <Table.Header className="text-xs text-muted-foreground"><Table.Row><Table.Head className="py-1">Field</Table.Head><Table.Head>Value</Table.Head><Table.Head><span className="sr-only">Actions</span></Table.Head></Table.Row></Table.Header>
          <Table.Body>
            {fields.map(([f, v]) => {
              const cur = edits[f] ?? binText(v);
              return (
                <Table.Row key={f} className="border-t border-border">
                  <Table.Cell className="py-1.5 pr-2 font-mono text-xs">{f}</Table.Cell>
                  <Table.Cell><Input size="sm" aria-label={`Value of ${f}`} className="w-full font-mono text-xs" value={cur} onChange={(e) => setEdits({ ...edits, [f]: e.target.value })} /></Table.Cell>
                  <Table.Cell className="space-x-1 whitespace-nowrap pl-2 text-right">
                    <Button aria-label={`Save ${f}`} disabled={cur === binText(v)} onClick={() => void act(async () => { await kvHashSet(ns, data.key, { [f]: binFrom(cur) }); setEdits((e) => omit(e, f)); })}>Save</Button>
                    <Button tone="danger" aria-label={`Delete field ${f}`} onClick={() => void act(() => kvHashDelete(ns, data.key, [f]))}>Delete</Button>
                  </Table.Cell>
                </Table.Row>
              );
            })}
          </Table.Body>
        </Table>
      )}
      <form className="flex flex-wrap gap-2 text-sm" onSubmit={(e) => { e.preventDefault(); void act(async () => { await kvHashSet(ns, data.key, { [name]: binFrom(val) }); setName(""); setVal(""); }); }}>
        <Input size="sm" aria-label="New field name" className="w-36" placeholder="field" value={name} onChange={(e) => setName(e.target.value)} />
        <Input size="sm" aria-label="New field value" className="w-48" placeholder="value" value={val} onChange={(e) => setVal(e.target.value)} />
        <Button type="submit" disabled={!name}>Add field</Button>
      </form>
    </div>
  );
}

function ListEditor({ ns, data, act }: EditorProps) {
  const items = data.value as Bin[];
  const [val, setVal] = useState("");
  const [editing, setEditing] = useState<number | null>(null);
  const [draft, setDraft] = useState("");
  return (
    <div className="space-y-3">
      {items.length === 0 && <EmptyState compact title="The list is empty" why="Push a value below." />}
      {items.length > 0 && (
        <ol aria-label="List items" className="divide-y divide-border rounded-lg border border-border text-sm">
          {items.map((it, i) => (
            <li key={i} className="flex items-center gap-2 px-2 py-1.5"><span className="w-8 shrink-0 text-right font-mono text-xs tabular-nums text-muted-foreground">{i}</span>{editing === i ? (
                <>
                  <Input size="sm" aria-label={`Item ${i} value`} className="flex-1 font-mono text-xs" value={draft} onChange={(e) => setDraft(e.target.value)} />
                  <Button tone="primary" aria-label={`Save item ${i}`} onClick={() => void act(async () => { await kvListSet(ns, data.key, i, binFrom(draft)); setEditing(null); })}>Save</Button>
                  <Button aria-label={`Cancel edit ${i}`} onClick={() => setEditing(null)}>Cancel</Button>
                </>
              ) : (
                <>
                  <span className="flex-1 break-all font-mono text-xs">{binText(it)}</span>
                  <Button aria-label={`Edit item ${i}`} onClick={() => { setDraft(binText(it)); setEditing(i); }}>Edit</Button>
                </>
              )}
              <Button tone="danger" aria-label={`Remove item ${i}`} onClick={() => void act(() => kvListRemove(ns, data.key, it, 1))}>Remove</Button>
            </li>
          ))}
        </ol>
      )}
      <div className="flex flex-wrap items-center gap-2 text-sm">
        <Input size="sm" aria-label="List value" className="w-48" placeholder="value" value={val} onChange={(e) => setVal(e.target.value)} />
        <Button disabled={!val} onClick={() => void act(async () => { await kvListPush(ns, data.key, "left", [binFrom(val)]); setVal(""); })}>Push left</Button>
        <Button disabled={!val} onClick={() => void act(async () => { await kvListPush(ns, data.key, "right", [binFrom(val)]); setVal(""); })}>Push right</Button>
        <Button onClick={() => void act(() => kvListPop(ns, data.key, "left"))}>Pop left</Button>
        <Button onClick={() => void act(() => kvListPop(ns, data.key, "right"))}>Pop right</Button>
      </div>
    </div>
  );
}

function SetEditor({ ns, data, act }: EditorProps) {
  const members = data.value as Bin[];
  const [val, setVal] = useState("");
  return (
    <div className="space-y-3">
      {members.length === 0 && <EmptyState compact title="The set is empty" why="Add a member below." />}
      {members.length > 0 && (
        <ul aria-label="Set members" className="divide-y divide-border rounded-lg border border-border text-sm">
          {members.map((m) => (
            <li key={binText(m)} className="flex items-center gap-2 px-3 py-1.5"><span className="flex-1 break-all font-mono text-xs">{binText(m)}</span>
              <Button tone="danger" aria-label={`Remove member ${binText(m)}`} onClick={() => void act(() => kvSetRemove(ns, data.key, [m]))}>Remove</Button>
            </li>
          ))}
        </ul>
      )}
      <form className="flex gap-2 text-sm" onSubmit={(e) => { e.preventDefault(); void act(async () => { await kvSetAdd(ns, data.key, [binFrom(val)]); setVal(""); }); }}>
        <Input size="sm" aria-label="New member" className="w-48" placeholder="member" value={val} onChange={(e) => setVal(e.target.value)} />
        <Button type="submit" disabled={!val}>Add member</Button>
      </form>
    </div>
  );
}

function ZSetEditor({ ns, data, act }: EditorProps) {
  const rows = data.value as ZMember[];
  const [scores, setScores] = useState<Record<string, string>>({});
  const [member, setMember] = useState("");
  const [score, setScore] = useState("0");
  return (
    <div className="space-y-3">
      {rows.length === 0 && <EmptyState compact title="The sorted set is empty" why="Add a member with a score below." />}
      {rows.length > 0 && (
        <Table aria-label="Sorted set members" density="compact" className="w-full text-left text-sm">
          <Table.Header className="text-xs text-muted-foreground"><Table.Row><Table.Head className="py-1">Member</Table.Head><Table.Head>Score</Table.Head><Table.Head><span className="sr-only">Actions</span></Table.Head></Table.Row></Table.Header>
          <Table.Body>
            {rows.map((r) => {
              const m = binText(r.member);
              const cur = scores[m] ?? String(r.score ?? "");
              return (
                <Table.Row key={m} className="border-t border-border">
                  <Table.Cell className="py-1.5 pr-2 font-mono text-xs">{m}</Table.Cell>
                  <Table.Cell><Input size="sm" aria-label={`Score of ${m}`} className="w-28 font-mono tabular-nums" value={cur} onChange={(e) => setScores({ ...scores, [m]: e.target.value })} /></Table.Cell>
                  <Table.Cell className="space-x-1 whitespace-nowrap pl-2 text-right">
                    <Button aria-label={`Save score of ${m}`} disabled={cur === String(r.score) || !Number.isFinite(Number(cur)) || cur === ""} onClick={() => void act(async () => { await kvZAdd(ns, data.key, [{ member: r.member, score: Number(cur) }]); setScores((e) => omit(e, m)); })}>Save</Button>
                    <Button tone="danger" aria-label={`Remove ${m}`} onClick={() => void act(() => kvZRemove(ns, data.key, [r.member]))}>Remove</Button>
                  </Table.Cell>
                </Table.Row>
              );
            })}
          </Table.Body>
        </Table>
      )}
      <form className="flex flex-wrap gap-2 text-sm" onSubmit={(e) => { e.preventDefault(); void act(async () => { await kvZAdd(ns, data.key, [{ member: binFrom(member), score: Number(score) }]); setMember(""); }); }}>
        <Input size="sm" aria-label="New member" className="w-44" placeholder="member" value={member} onChange={(e) => setMember(e.target.value)} />
        <Input size="sm" aria-label="New score" className="w-24" value={score} onChange={(e) => setScore(e.target.value)} />
        <Button type="submit" disabled={!member || !Number.isFinite(Number(score)) || score === ""}>Add member</Button>
      </form>
    </div>
  );
}

function StreamEditor({ ns, data, act }: EditorProps) {
  const entries = data.value as StreamEntry[];
  const [text, setText] = useState("");
  const [error, setError] = useState<string>();
  const add = () => {
    setError(undefined);
    let fields: Record<string, string>;
    try { fields = parseFields(text); } catch (e) { setError(e instanceof Error ? e.message : String(e)); return; }
    void act(async () => { await kvStreamAdd(ns, data.key, Object.fromEntries(Object.entries(fields).map(([k, v]) => [k, binFrom(v)]))); setText(""); });
  };
  return (
    <div className="space-y-3">
      {entries.length === 0 && <EmptyState compact title="The stream has no entries" why="Add an entry below." />}
      {entries.length > 0 && (
        <ul aria-label="Stream entries" className="space-y-2 text-sm">
          {entries.map((e) => {
            const id = binText(e.id);
            return (
              <li key={id} className="rounded-lg border border-border px-3 py-2">
                <div className="flex items-center justify-between gap-2"><span className="font-mono text-xs font-medium text-primary">{id}</span>
                  <Button tone="danger" aria-label={`Delete entry ${id}`} onClick={() => void act(() => kvStreamDelete(ns, data.key, [id]))}>Delete</Button></div>
                <dl className="mt-1 grid grid-cols-[8rem_minmax(0,1fr)] gap-x-3 gap-y-0.5 font-mono text-xs">{Object.entries(e.fields).map(([k, v]) => (<Fragment key={k}><dt className="truncate text-muted-foreground" title={k}>{k}</dt><dd className="break-all">{binText(v)}</dd></Fragment>))}</dl>
              </li>
            );
          })}
        </ul>
      )}
      <div className="space-y-2 text-sm">
        <Textarea size="sm" autoResize={false} aria-label="New entry fields" className="block h-20 w-full font-mono text-xs" placeholder={"name=value\nother=value"} value={text} onChange={(e) => setText(e.target.value)} />
        {error && <p role="alert" className="text-destructive">{error}</p>}
        <Button disabled={!text.trim()} onClick={add}>Add entry</Button>
      </div>
    </div>
  );
}
