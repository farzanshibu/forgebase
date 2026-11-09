import { useEffect, useRef, useState } from "react";
import { binText, kvChannels, kvPublish, kvSubscribe } from "../../api";
import type { PubSubMessage } from "../../api";
import { Button, ErrorView, Panel, inputCls } from "../../components/ui";
import { useLoad } from "../../hooks";

interface Line { id: number; at: string; channel: string; text: string }

/** Pub/Sub inspector: listen to a channel over SSE (`GET /kv/{ns}/subscribe/{channel}`) and publish to it. */
export default function PubSub({ ns }: { ns: string }) {
  const [channel, setChannel] = useState("");
  const [listening, setListening] = useState<string | null>(null);
  const [lines, setLines] = useState<Line[]>([]);
  const [lagged, setLagged] = useState(0);
  const [error, setError] = useState<unknown>();
  const [message, setMessage] = useState("");
  const [receivers, setReceivers] = useState<number>();
  const channels = useLoad((s) => kvChannels(ns, s), `kv-channels:${ns}`);
  const ctrl = useRef<AbortController | null>(null);
  const seq = useRef(0);

  const stop = () => { ctrl.current?.abort(); ctrl.current = null; setListening(null); };
  useEffect(() => () => ctrl.current?.abort(), []);

  const start = (e: React.FormEvent) => {
    e.preventDefault();
    const ch = channel.trim();
    if (!ch) return;
    stop();
    setError(undefined); setLagged(0);
    const c = new AbortController();
    ctrl.current = c;
    setListening(ch);
    kvSubscribe(ns, ch, (m: PubSubMessage) => setLines((l) => [{ id: ++seq.current, at: new Date().toISOString().slice(11, 19), channel: m.channel, text: binText(m.message) }, ...l].slice(0, 200)), c.signal, (n) => setLagged((x) => x + n))
      .then(() => { if (ctrl.current === c) { ctrl.current = null; setListening(null); } })
      .catch((err: unknown) => { if (ctrl.current === c) { ctrl.current = null; setListening(null); setError(err); } });
  };
  const publish = async (e: React.FormEvent) => {
    e.preventDefault();
    setError(undefined);
    try { setReceivers((await kvPublish(ns, channel.trim(), message)).receivers); setMessage(""); } catch (err) { setError(err); }
  };
  return (
    <div className="space-y-4">
      <Panel title={`Channel (namespace ${ns})`}>
        <form onSubmit={start} className="flex flex-wrap items-end gap-3 text-sm">
          <label>Channel<input className={`${inputCls} ml-2 w-48`} value={channel} onChange={(e) => setChannel(e.target.value)} /></label>
          {listening ? <Button onClick={stop}>Stop listening</Button> : <Button type="submit" tone="primary" disabled={!channel.trim()}>Listen</Button>}
          {listening && <span role="status" className="text-emerald-300">Listening on {listening}</span>}
        </form>
        <div className="mt-3"><ErrorView error={error} /></div>
      </Panel>
      <Panel title="Active channels" actions={<Button onClick={channels.reload}>Refresh</Button>}>
        <ErrorView error={channels.error} />
        {channels.data && channels.data.length === 0 && <p className="text-sm text-zinc-400">No channel has a subscriber right now.</p>}
        {channels.data && channels.data.length > 0 && (
          <ul aria-label="Active channels" className="space-y-1 text-sm">
            {channels.data.map((c) => (
              <li key={c.name} className="flex items-center gap-3">
                <button type="button" className="font-mono text-amber-300 hover:underline" onClick={() => setChannel(c.name)}>{c.name}</button>
                <span className="text-xs text-zinc-400">{c.subscribers} subscriber{c.subscribers === 1 ? "" : "s"}</span>
              </li>
            ))}
          </ul>
        )}
      </Panel>
      <Panel title="Publish">
        <form onSubmit={(e) => void publish(e)} className="flex flex-wrap items-end gap-3 text-sm">
          <label>Message<input className={`${inputCls} ml-2 w-72`} value={message} onChange={(e) => setMessage(e.target.value)} /></label>
          <Button type="submit" disabled={!channel.trim() || !message}>Publish</Button>
          {receivers !== undefined && <span role="status" className="text-zinc-400">Delivered to {receivers} subscriber{receivers === 1 ? "" : "s"}.</span>}
        </form>
      </Panel>
      <Panel title="Messages" actions={<Button onClick={() => setLines([])} disabled={lines.length === 0}>Clear</Button>}>
        {lagged > 0 && <p className="mb-2 text-sm text-amber-300">{lagged} message(s) were dropped because the listener fell behind.</p>}
        {lines.length === 0 ? <p className="text-sm text-zinc-400">No messages yet.</p> : (
          <ul aria-label="Received messages" className="space-y-1 font-mono text-xs">
            {lines.map((l) => <li key={l.id}><span className="text-zinc-500">{l.at}</span> <span className="text-amber-300">{l.channel}</span> {l.text}</li>)}
          </ul>
        )}
      </Panel>
    </div>
  );
}
