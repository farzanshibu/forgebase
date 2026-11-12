import { useEffect, useId, useRef, useState } from "react";
import { ackQueueMessages, listQueueSubscriptions, listQueueTopics, nackQueueMessages, publishQueueMessages, streamQueueSubscription } from "../../api";
import type { ReceivedMessage } from "../../api";
import { Button, ErrorView } from "../../components/ui";
import { Dot, Section } from "../jobs/ops";
import { EmptyState } from "../../components/EmptyState";
import { HelpTip } from "../../components/HelpTip";
import { useLoad } from "../../hooks";
import { parseJson } from "./util";
import { Input } from "panelui-web/input";
import { Sel } from "../../components/controls";

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
  const uid = useId();

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
  const pull = (subs.data ?? []).filter((s) => !s.push);

  return (
    <div className="space-y-4">
      <Section
        title="Listen"
        description={<span className="inline-flex flex-wrap items-center gap-1">Watch a subscription <HelpTip term="subscription" /> deliver messages live, and acknowledge each one. Record messages show the subscription owner&apos;s view; a message the owner may not see is withheld and does not appear.</span>}
      >
        <form onSubmit={start} className="flex flex-wrap items-end gap-3 text-sm">
          <div className="min-w-56 flex-1 sm:max-w-sm">
            <span className="mb-1.5 block font-medium">Subscription</span>
            <Sel aria-label="Subscription" className="w-full" value={sub} onChange={setSub} disabled={!!live} placeholder={pull.length ? "Choose a pull subscription" : "No pull subscriptions yet"} options={pull.map((s) => ({ value: s.name, label: `${s.name} (${s.topic})` }))} />
          </div>
          <div>
            <span className="mb-1.5 flex items-center gap-1 font-medium"><label htmlFor={`${uid}flow`}>Most in flight</label><HelpTip term="ack_deadline" /></span>
            <Input id={`${uid}flow`} size="sm" className="w-24 tabular-nums" inputMode="numeric" value={flow} onChange={(e) => setFlow(e.target.value)} disabled={!!live} />
          </div>
          {live ? <Button onClick={stop}>Stop listening</Button> : <Button type="submit" tone="primary" disabled={!sub}>Listen</Button>}
          {live && <span role="status" className="inline-flex items-center gap-2 self-center text-success"><Dot tone="ok" pulse />Listening on {live}</span>}
        </form>
        <p className="mt-3 text-xs text-muted-foreground">Deliveries are leased to this page until you ack or nack them; unacknowledged ones return after the ack deadline.</p>
        <div className="mt-3"><ErrorView error={error ?? subs.error} /></div>
      </Section>
      <Section
        title="Deliveries"
        count={lines.length || undefined}
        description={lines.length ? `${held.length} waiting for an ack. Newest first; the last 200 are kept.` : undefined}
        actions={<><Button disabled={held.length === 0} onClick={() => void settle(held, "ack")}>Ack all</Button><Button onClick={() => setLines([])} disabled={lines.length === 0}>Clear</Button></>}
      >
        {lines.length === 0 ? <EmptyState compact title="No deliveries yet" why="Choose a pull subscription and press Listen. Publish a message below to see one arrive." /> : (
          <ul aria-label="Deliveries" className="divide-y divide-border rounded-lg border border-border text-sm">
            {lines.map((l) => (
              <li key={l.n} className="px-3 py-2.5">
                <div className="flex flex-wrap items-center gap-x-3 gap-y-1">
                  <span className={`inline-flex items-center gap-1 rounded-full border px-2 py-0.5 text-xs font-medium ${STATE_CLASS[l.state]}`}>{l.state}</span>
                  <span className="tabular-nums text-muted-foreground">seq {l.message.seq}</span>
                  <span className="min-w-0 truncate font-mono text-xs" title={l.message.id}>{l.message.id}</span>
                  <span className="text-xs text-muted-foreground">attempt {l.delivery_attempt}</span>
                  {l.state === "held" && (
                    <span className="ml-auto flex gap-2">
                      <Button aria-label={`Ack ${l.message.id}`} onClick={() => void settle([l.ack_id], "ack")}>Ack</Button>
                      <Button tone="danger" aria-label={`Nack ${l.message.id}`} onClick={() => void settle([l.ack_id], "nack")}>Nack</Button>
                    </span>
                  )}
                </div>
                <pre className="mt-1.5 overflow-x-auto rounded-md bg-muted/40 px-2 py-1.5 font-mono text-xs">{JSON.stringify(l.message.data)}</pre>
              </li>
            ))}
          </ul>
        )}
      </Section>
      <Publish topics={(topics.data ?? []).filter((t) => !t.source).map((t) => t.name)} />
    </div>
  );
}

const STATE_CLASS: Record<Line["state"], string> = {
  held: "border-warning/40 bg-warning/10 text-warning",
  acked: "border-success/40 bg-success/10 text-success",
  nacked: "border-destructive/40 bg-destructive/10 text-destructive",
};

function Publish({ topics }: { topics: string[] }) {
  const [topic, setTopic] = useState("");
  const [data, setData] = useState('{"hello":"world"}');
  const [error, setError] = useState<unknown>();
  const [last, setLast] = useState("");
  const uid = useId();
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
    <Section title="Publish" description="Send a message to a topic and watch it arrive above. Topics that carry record changes are not offered.">
      <form onSubmit={(e) => void submit(e)} className="flex flex-wrap items-end gap-3 text-sm">
        <div className="min-w-48">
          <span className="mb-1.5 block font-medium">Topic</span>
          <Sel aria-label="Topic" className="w-full" value={topic} onChange={setTopic} placeholder={topics.length ? "Choose a topic" : "No topics yet"} options={topics.map((t) => ({ value: t, label: t }))} />
        </div>
        <div className="min-w-64 flex-1">
          <label htmlFor={`${uid}data`} className="mb-1.5 block font-medium">Data (JSON)</label>
          <Input id={`${uid}data`} size="sm" spellCheck={false} className="w-full font-mono text-xs" value={data} onChange={(e) => setData(e.target.value)} />
        </div>
        <Button type="submit" tone="primary" disabled={!topic}>Publish</Button>
      </form>
      {last && <p className="mt-2 text-sm text-success">{last}</p>}
      <div className="mt-3"><ErrorView error={error} /></div>
    </Section>
  );
}
