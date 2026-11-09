import { useState } from "react";
import {
  ApiError, binFrom, binText, kvClearTtl, kvDelete, kvExec, kvGet, kvHashDelete, kvHashSet, kvListPop, kvListPush, kvSetAdd, kvSetRemove, kvSetString, kvSetTtl,
  kvStreamAdd, kvZAdd, kvZRemove,
} from "../../api";
import type { Bin, KvKey, StreamEntry, ZMember } from "../../api";
import { Button, ConfirmButton, ErrorView, Loading, Panel, inputCls } from "../../components/ui";
import { formatBytes } from "../../format";
import { useLoad } from "../../hooks";
import { TTL_UNITS, formatTtl, parseFields } from "./util";

type Act = (f: () => Promise<unknown>) => Promise<void>;
interface EditorProps { ns: string; data: KvKey; act: Act }

export default function KeyViewer({ ns, keyName, onGone, onChanged }: { ns: string; keyName: string; onGone: () => void; onChanged: () => void }) {
  const kv = useLoad((s) => kvGet(ns, keyName, 1000, s), `kv:${ns}:${keyName}`);
  const [error, setError] = useState<unknown>();
  const act: Act = async (f) => {
    setError(undefined);
    try { await f(); kv.reload(); onChanged(); } catch (e) { setError(e); }
  };
  if (kv.error instanceof ApiError && kv.error.status === 404) return <Panel><p className="text-sm text-zinc-400">Key <span className="font-mono">{keyName}</span> no longer exists.</p><div className="mt-2"><Button onClick={onGone}>Back to keys</Button></div></Panel>;
  const d = kv.data;
  return (
    <div className="space-y-4" aria-label="Key details">
      <Panel title={keyName} actions={d && <span className="rounded bg-zinc-800 px-2 py-0.5 text-xs text-zinc-300">{d.type}</span>}>
        <ErrorView error={kv.error} />
        {kv.loading && !kv.error && <Loading what="Loading key" />}
        {d && (
          <>
            <p className="text-sm text-zinc-400" aria-label="Key summary">
              {d.type === "string" ? `${formatBytes(d.size)}` : `${d.size} ${d.type === "hash" ? "fields" : d.type === "stream" ? "entries" : d.type === "list" ? "items" : "members"}`}
              {" · TTL "}<span aria-label="Remaining TTL">{formatTtl(d.ttl_ms)}</span>
              {d.truncated && <span className="text-amber-300"> · showing the first 1000 only</span>}
            </p>
            <div className="mt-3"><Ttl ns={ns} data={d} act={act} /></div>
            <div className="mt-3"><ConfirmButton label="Delete key" onConfirm={() => void (async () => { try { await kvDelete(ns, keyName); onGone(); } catch (e) { setError(e); } })()} /></div>
          </>
        )}
        <div className="mt-2"><ErrorView error={error} /></div>
      </Panel>
      {d && (
        <Panel title="Value">
          {d.type === "string" && <StringEditor ns={ns} data={d} act={act} />}
          {d.type === "hash" && <HashEditor ns={ns} data={d} act={act} />}
          {d.type === "list" && <ListEditor ns={ns} data={d} act={act} />}
          {d.type === "set" && <SetEditor ns={ns} data={d} act={act} />}
          {d.type === "zset" && <ZSetEditor ns={ns} data={d} act={act} />}
          {d.type === "stream" && <StreamEditor ns={ns} data={d} act={act} />}
        </Panel>
      )}
    </div>
  );
}

function Ttl({ ns, data, act }: EditorProps) {
  const [amount, setAmount] = useState("60");
  const [unit, setUnit] = useState(1000);
  const ms = Math.round(Number(amount) * unit);
  return (
    <div className="flex flex-wrap items-end gap-2 text-sm">
      <label>Expire in<input aria-label="TTL amount" type="number" min="1" className={`${inputCls} ml-2 w-24`} value={amount} onChange={(e) => setAmount(e.target.value)} /></label>
      <select aria-label="TTL unit" className={inputCls} value={unit} onChange={(e) => setUnit(Number(e.target.value))}>{TTL_UNITS.map((u) => <option key={u.ms} value={u.ms}>{u.label}</option>)}</select>
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
      <textarea aria-label="String value" className={`${inputCls} block h-40 w-full font-mono text-xs`} value={text} onChange={(e) => setText(e.target.value)} />
      <p className="text-xs text-zinc-500">Binary values show as <span className="font-mono">base64:…</span>; keep the prefix to store bytes. Saving keeps the TTL.</p>
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
      {fields.length === 0 && <p className="text-sm text-zinc-400">No fields.</p>}
      {fields.length > 0 && (
        <table aria-label="Hash fields" className="w-full text-left text-sm">
          <thead className="text-xs uppercase tracking-wide text-zinc-500"><tr><th className="py-1">Field</th><th>Value</th><th /></tr></thead>
          <tbody>
            {fields.map(([f, v]) => {
              const cur = edits[f] ?? binText(v);
              return (
                <tr key={f} className="border-t border-zinc-800">
                  <td className="py-1.5 pr-2 font-mono text-xs">{f}</td>
                  <td><input aria-label={`Value of ${f}`} className={`${inputCls} w-full font-mono text-xs`} value={cur} onChange={(e) => setEdits({ ...edits, [f]: e.target.value })} /></td>
                  <td className="space-x-1 whitespace-nowrap pl-2 text-right">
                    <Button aria-label={`Save ${f}`} disabled={cur === binText(v)} onClick={() => void act(async () => { await kvHashSet(ns, data.key, { [f]: binFrom(cur) }); setEdits(({ [f]: _, ...rest }) => rest); })}>Save</Button>
                    <Button tone="danger" aria-label={`Delete field ${f}`} onClick={() => void act(() => kvHashDelete(ns, data.key, [f]))}>Delete</Button>
                  </td>
                </tr>
              );
            })}
          </tbody>
        </table>
      )}
      <form className="flex flex-wrap gap-2 text-sm" onSubmit={(e) => { e.preventDefault(); void act(async () => { await kvHashSet(ns, data.key, { [name]: binFrom(val) }); setName(""); setVal(""); }); }}>
        <input aria-label="New field name" className={`${inputCls} w-36`} placeholder="field" value={name} onChange={(e) => setName(e.target.value)} />
        <input aria-label="New field value" className={`${inputCls} w-48`} placeholder="value" value={val} onChange={(e) => setVal(e.target.value)} />
        <Button type="submit" disabled={!name}>Add field</Button>
      </form>
    </div>
  );
}

function ListEditor({ ns, data, act }: EditorProps) {
  const items = data.value as Bin[];
  const [val, setVal] = useState("");
  return (
    <div className="space-y-3">
      {items.length === 0 && <p className="text-sm text-zinc-400">The list is empty.</p>}
      {items.length > 0 && (
        <ol aria-label="List items" className="space-y-1 text-sm">
          {items.map((it, i) => (
            <li key={i} className="flex items-center gap-2"><span className="w-8 text-right text-xs text-zinc-500">{i}</span><span className="flex-1 break-all font-mono text-xs">{binText(it)}</span>
              <Button tone="danger" aria-label={`Remove item ${i}`} onClick={() => void act(() => kvExec(ns, ["LREM", data.key, "1", binText(it)]))}>Remove</Button>
            </li>
          ))}
        </ol>
      )}
      <div className="flex flex-wrap items-center gap-2 text-sm">
        <input aria-label="List value" className={`${inputCls} w-48`} placeholder="value" value={val} onChange={(e) => setVal(e.target.value)} />
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
      {members.length === 0 && <p className="text-sm text-zinc-400">The set is empty.</p>}
      {members.length > 0 && (
        <ul aria-label="Set members" className="space-y-1 text-sm">
          {members.map((m) => (
            <li key={binText(m)} className="flex items-center gap-2"><span className="flex-1 break-all font-mono text-xs">{binText(m)}</span>
              <Button tone="danger" aria-label={`Remove member ${binText(m)}`} onClick={() => void act(() => kvSetRemove(ns, data.key, [m]))}>Remove</Button>
            </li>
          ))}
        </ul>
      )}
      <form className="flex gap-2 text-sm" onSubmit={(e) => { e.preventDefault(); void act(async () => { await kvSetAdd(ns, data.key, [binFrom(val)]); setVal(""); }); }}>
        <input aria-label="New member" className={`${inputCls} w-48`} placeholder="member" value={val} onChange={(e) => setVal(e.target.value)} />
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
      {rows.length === 0 && <p className="text-sm text-zinc-400">The sorted set is empty.</p>}
      {rows.length > 0 && (
        <table aria-label="Sorted set members" className="w-full text-left text-sm">
          <thead className="text-xs uppercase tracking-wide text-zinc-500"><tr><th className="py-1">Member</th><th>Score</th><th /></tr></thead>
          <tbody>
            {rows.map((r) => {
              const m = binText(r.member);
              const cur = scores[m] ?? String(r.score ?? "");
              return (
                <tr key={m} className="border-t border-zinc-800">
                  <td className="py-1.5 pr-2 font-mono text-xs">{m}</td>
                  <td><input aria-label={`Score of ${m}`} className={`${inputCls} w-28`} value={cur} onChange={(e) => setScores({ ...scores, [m]: e.target.value })} /></td>
                  <td className="space-x-1 whitespace-nowrap pl-2 text-right">
                    <Button aria-label={`Save score of ${m}`} disabled={cur === String(r.score) || !Number.isFinite(Number(cur)) || cur === ""} onClick={() => void act(async () => { await kvZAdd(ns, data.key, [{ member: r.member, score: Number(cur) }]); setScores(({ [m]: _, ...rest }) => rest); })}>Save</Button>
                    <Button tone="danger" aria-label={`Remove ${m}`} onClick={() => void act(() => kvZRemove(ns, data.key, [r.member]))}>Remove</Button>
                  </td>
                </tr>
              );
            })}
          </tbody>
        </table>
      )}
      <form className="flex flex-wrap gap-2 text-sm" onSubmit={(e) => { e.preventDefault(); void act(async () => { await kvZAdd(ns, data.key, [{ member: binFrom(member), score: Number(score) }]); setMember(""); }); }}>
        <input aria-label="New member" className={`${inputCls} w-44`} placeholder="member" value={member} onChange={(e) => setMember(e.target.value)} />
        <input aria-label="New score" className={`${inputCls} w-24`} value={score} onChange={(e) => setScore(e.target.value)} />
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
      {entries.length === 0 && <p className="text-sm text-zinc-400">The stream has no entries.</p>}
      {entries.length > 0 && (
        <ul aria-label="Stream entries" className="space-y-2 text-sm">
          {entries.map((e) => {
            const id = binText(e.id);
            return (
              <li key={id} className="rounded border border-zinc-800 p-2">
                <div className="flex items-center justify-between"><span className="font-mono text-xs text-amber-300">{id}</span>
                  <Button tone="danger" aria-label={`Delete entry ${id}`} onClick={() => void act(() => kvExec(ns, ["XDEL", data.key, id]))}>Delete</Button></div>
                <dl className="mt-1 grid grid-cols-[8rem_1fr] gap-y-0.5 font-mono text-xs">{Object.entries(e.fields).map(([k, v]) => (<><dt key={`k${k}`} className="text-zinc-500">{k}</dt><dd key={`v${k}`}>{binText(v)}</dd></>))}</dl>
              </li>
            );
          })}
        </ul>
      )}
      <div className="space-y-2 text-sm">
        <textarea aria-label="New entry fields" className={`${inputCls} block h-20 w-full font-mono text-xs`} placeholder={"name=value\nother=value"} value={text} onChange={(e) => setText(e.target.value)} />
        {error && <p role="alert" className="text-red-300">{error}</p>}
        <Button disabled={!text.trim()} onClick={add}>Add entry</Button>
      </div>
    </div>
  );
}
