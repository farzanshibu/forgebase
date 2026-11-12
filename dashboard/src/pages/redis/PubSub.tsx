import { useEffect, useRef, useState } from "react";
import { binText, kvChannels, kvPublish, kvSubscribe } from "../../api";
import type { PubSubMessage } from "../../api";
import { EmptyState } from "../../components/EmptyState";
import { Badge } from "panelui-web/badge";
import { FormField } from "../../components/FormField";
import { Button, ErrorView } from "../../components/ui";
import { useLoad } from "../../hooks";
import { Input } from "panelui-web/input";

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
  const card = "overflow-hidden rounded-xl border border-border bg-card";
  const head = "flex items-center justify-between gap-2 border-b border-border px-4 py-2.5";
  return (
    <div className="grid items-start gap-4 lg:grid-cols-[minmax(0,22rem)_minmax(0,1fr)]">
      <div className="space-y-4">
        <section aria-labelledby="kv-ps-channel" className={card}>
          <header className={head}>
            <h2 id="kv-ps-channel" className="text-sm font-medium">Listen to a channel</h2>
            {listening
              ? <Badge variant="success" size="sm" dot="pulse"><span role="status">Listening on {listening}</span></Badge>
              : <Badge variant="secondary" size="sm">Not listening</Badge>}
          </header>
          <form onSubmit={start} className="space-y-3 px-4 py-3 text-sm">
            <FormField label="Channel" help={`In namespace ${ns}. Listen, then publish to see messages arrive.`}>
              {(p) => <Input {...p} size="sm" className="w-full font-mono" placeholder="news" value={channel} onChange={(e) => setChannel(e.target.value)} />}
            </FormField>
            {listening ? <Button onClick={stop}>Stop listening</Button> : <Button type="submit" tone="primary" disabled={!channel.trim()}>Listen</Button>}
            <ErrorView error={error} />
          </form>
        </section>
        <section aria-labelledby="kv-ps-publish" className={card}>
          <header className={head}><h2 id="kv-ps-publish" className="text-sm font-medium">Publish</h2></header>
          <form onSubmit={(e) => void publish(e)} className="space-y-3 px-4 py-3 text-sm">
            <FormField label="Message" help={channel.trim() ? <>Sent to <span className="font-mono">{channel.trim()}</span>.</> : "Enter a channel above first."}>
              {(p) => <Input {...p} size="sm" className="w-full" value={message} onChange={(e) => setMessage(e.target.value)} />}
            </FormField>
            <div className="flex flex-wrap items-center gap-3">
              <Button type="submit" disabled={!channel.trim() || !message}>Publish</Button>
              {receivers !== undefined && <span role="status" className="text-xs text-muted-foreground">Delivered to {receivers} subscriber{receivers === 1 ? "" : "s"}.</span>}
            </div>
          </form>
        </section>
        <section aria-labelledby="kv-ps-active" className={card}>
          <header className={head}>
            <h2 id="kv-ps-active" className="text-sm font-medium">Active channels</h2>
            <Button onClick={channels.reload}>Refresh</Button>
          </header>
          <div className="px-4 py-3">
            <ErrorView error={channels.error} />
            {channels.data && channels.data.length === 0 && <EmptyState compact title="No active channels" why="A channel appears here while something is listening to it. Listen above to start one." />}
            {channels.data && channels.data.length > 0 && (
              <ul aria-label="Active channels" className="divide-y divide-border text-sm">
                {channels.data.map((c) => (
                  <li key={c.name} className="flex items-center justify-between gap-3 py-1.5">
                    <button type="button" className="truncate font-mono text-primary hover:underline" title="Use this channel" onClick={() => setChannel(c.name)}>{c.name}</button>
                    <span className="shrink-0 text-xs tabular-nums text-muted-foreground">{c.subscribers} subscriber{c.subscribers === 1 ? "" : "s"}</span>
                  </li>
                ))}
              </ul>
            )}
          </div>
        </section>
      </div>
      <section aria-labelledby="kv-ps-messages" className={card}>
        <header className={head}>
          <h2 id="kv-ps-messages" className="text-sm font-medium">Messages{lines.length > 0 && <span className="ml-2 text-xs font-normal tabular-nums text-muted-foreground">{lines.length}</span>}</h2>
          <Button onClick={() => setLines([])} disabled={lines.length === 0}>Clear</Button>
        </header>
        <div className="p-4">
          {lagged > 0 && <p className="mb-2 rounded-lg bg-warning/10 px-3 py-2 text-sm text-warning">{lagged} message(s) were dropped because the listener fell behind.</p>}
          {lines.length === 0 ? <EmptyState compact title="No messages yet" why="Listen to a channel, then publish to it to see messages arrive. The newest message is on top." /> : (
            <ul aria-label="Received messages" className="max-h-[32rem] divide-y divide-border overflow-auto rounded-lg border border-border bg-muted/40 font-mono text-xs">
              {lines.map((l) => (
                <li key={l.id} className="grid grid-cols-[4.5rem_minmax(0,8rem)_minmax(0,1fr)] gap-3 px-3 py-1.5">
                  <span className="tabular-nums text-muted-foreground">{l.at}</span>
                  <span className="truncate text-primary" title={l.channel}>{l.channel}</span>
                  <span className="whitespace-pre-wrap break-all text-foreground">{l.text}</span>
                </li>
              ))}
            </ul>
          )}
        </div>
      </section>
    </div>
  );
}
