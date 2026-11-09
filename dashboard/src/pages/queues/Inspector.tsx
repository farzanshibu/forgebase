import { useEffect, useRef, useState } from "react";
import { ackQueueMessages, listQueueSubscriptions, listQueueTopics, nackQueueMessages, publishQueueMessages, streamQueueSubscription } from "../../api";
import type { ReceivedMessage } from "../../api";
import { Button, ErrorView, Panel, inputCls } from "../../components/ui";
import { useLoad } from "../../hooks";
import { parseJson } from "./util";

interface Line extends ReceivedMessage { n: number; state: "held" | "acked" | "nacked" }

/** Pub/Sub inspector: stream a subscription (server-sent events, with flow control), acknowledge each delivery, and publish to a topic. */
export default function Inspector() {
  const subs = useLoad((s) => listQueueSubscriptions(undefined, s), "inspect-subs");
  const topics = useLoad(listQueueTopics, "inspect-topics");
  const [sub, setSub] = useState("");
  const [flow, setFlow] = useState("10");
  const [live, setLive] = useState<string | null>(null);
  const [lines, setLines] = useState<Line[]>([]);
  const [error, setError] = useState<unknown>();
  const ctrl = useRef<AbortController | null>(null);
  const seq = useRef(0);

  const stop = () => { ctrl.current?.abort(); ctrl.current = null; setLive(null); };
  useEffect(() => () => ctrl.current?.abort(), []);

  function start(e: React.FormEvent) {
    e.preventDefault();
    if (!sub) return;
    stop();
    setError(undefined);
    const c = new AbortController();
    ctrl.current = c;
    setLive(sub);
    streamQueueSubscription(sub, (m) => setLines((l) => [{ ...m, n: ++seq.current, state: "held" as const }, ...l].slice(0, 200)), c.signal, { maxMessages: Math.max(1, Number(flow) || 10) })
      .then(() => { if (ctrl.current === c) { ctrl.current = null; setLive(null); } })
      .catch((err: unknown) => { if (ctrl.current === c) { ctrl.current = null; setLive(null); setError(err); } });
  }

  async function settle(ids: string[], how: "ack" | "nack") {
    if (!live || ids.length === 0) return;
    setError(undefined);
    try {
      await (how === "ack" ? ackQueueMessages(live, ids) : nackQueueMessages(live, ids));
      setLines((l) => l.map((x) => (ids.includes(x.ack_id) ? { ...x, state: how === "ack" ? "acked" : "nacked" } : x)));
    } catch (e) {
      setError(e);
    }
  }
  const held = lines.filter((l) => l.state === "held").map((l) => l.ack_id);

  return (
    <div className="space-y-4">
      <Panel title="Listen">
        <form onSubmit={start} className="flex flex-wrap items-end gap-3 text-sm">
          <label>Subscription
            <select className={`${inputCls} ml-2`} value={sub} onChange={(e) => setSub(e.target.value)}>
              <option value="">choose…</option>
              {(subs.data ?? []).filter((s) => !s.push).map((s) => <option key={s.name} value={s.name}>{s.name} ({s.topic})</option>)}
            </select>
          </label>
          <label>Most in flight<input className={`${inputCls} ml-2 w-16`} inputMode="numeric" value={flow} onChange={(e) => setFlow(e.target.value)} /></label>
          {live ? <Button onClick={stop}>Stop listening</Button> : <Button type="submit" tone="primary" disabled={!sub}>Listen</Button>}
          {live && <span role="status" className="text-emerald-300">Listening on {live}</span>}
        </form>
        <p className="mt-2 text-xs text-zinc-500">Deliveries are leased to this page until you ack or nack them; unacknowledged ones return after the ack deadline.</p>
        <div className="mt-3"><ErrorView error={error ?? subs.error} /></div>
      </Panel>
      <Publish topics={(topics.data ?? []).filter((t) => !t.source).map((t) => t.name)} />
      <Panel title="Deliveries" actions={<div className="flex gap-2"><Button disabled={held.length === 0} onClick={() => void settle(held, "ack")}>Ack all</Button><Button onClick={() => setLines([])} disabled={lines.length === 0}>Clear</Button></div>}>
        {lines.length === 0 ? <p className="text-sm text-zinc-400">No deliveries yet.</p> : (
          <ul aria-label="Deliveries" className="space-y-2 text-sm">
            {lines.map((l) => (
              <li key={l.n} className="rounded border border-zinc-800 p-2">
                <div className="flex flex-wrap items-center gap-x-3">
                  <span className="text-zinc-500">seq {l.message.seq}</span><span className="font-mono text-xs">{l.message.id}</span>
                  <span>attempt {l.delivery_attempt}</span>
                  <span className={l.state === "held" ? "text-amber-300" : "text-zinc-400"}>{l.state}</span>
                  {l.state === "held" && <>
                    <button type="button" className="text-emerald-300 hover:underline" aria-label={`Ack ${l.message.id}`} onClick={() => void settle([l.ack_id], "ack")}>Ack</button>
                    <button type="button" className="text-red-300 hover:underline" aria-label={`Nack ${l.message.id}`} onClick={() => void settle([l.ack_id], "nack")}>Nack</button>
                  </>}
                </div>
                <pre className="mt-1 overflow-x-auto font-mono text-xs">{JSON.stringify(l.message.data)}</pre>
              </li>
            ))}
          </ul>
        )}
      </Panel>
    </div>
  );
}

function Publish({ topics }: { topics: string[] }) {
  const [topic, setTopic] = useState("");
  const [data, setData] = useState('{"hello":"world"}');
  const [error, setError] = useState<unknown>();
  const [last, setLast] = useState("");
  async function submit(e: React.FormEvent) {
    e.preventDefault();
    setError(undefined);
    setLast("");
    const p = parseJson(data);
    if (!p.ok) return setError(new Error("The message data is not valid JSON."));
    try {
      const [r] = await publishQueueMessages(topic, [{ data: p.value }]);
      setLast(r ? `Published to ${topic} as sequence ${r.seq}.` : "");
    } catch (err) {
      setError(err);
    }
  }
  return (
    <Panel title="Publish">
      <form onSubmit={(e) => void submit(e)} className="flex flex-wrap items-end gap-3 text-sm">
        <label>Topic
          <select className={`${inputCls} ml-2`} value={topic} onChange={(e) => setTopic(e.target.value)}>
            <option value="">choose…</option>
            {topics.map((t) => <option key={t} value={t}>{t}</option>)}
          </select>
        </label>
        <label>Data (JSON)<input className={`${inputCls} ml-2 w-72 font-mono text-xs`} value={data} onChange={(e) => setData(e.target.value)} /></label>
        <Button type="submit" disabled={!topic}>Publish</Button>
        {last && <span role="status" className="text-emerald-300">{last}</span>}
      </form>
      <div className="mt-3"><ErrorView error={error} /></div>
    </Panel>
  );
}
