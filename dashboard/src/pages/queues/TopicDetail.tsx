import { Fragment, useEffect, useState } from "react";
import { Link, useNavigate, useParams } from "react-router-dom";
import { ApiError, createQueueSubscription, deleteQueueTopic, listQueueSubscriptions, publishQueueMessages, queueTopicMessages, queueTopicMetrics } from "../../api";
import type { QueueMessage, QueueSubscription } from "../../api";
import { Button, ConfirmButton, ErrorView, Loading, Panel, inputCls } from "../../components/ui";
import { useLoad, type LoadState } from "../../hooks";
import { when } from "../functions/util";
import { bytes, duration, parseAttrs, parseJson, pretty } from "./util";
import { Table } from "panelui-web/table";
import { Input } from "panelui-web/input";

const PAGE = 25;

export default function TopicDetail() {
  const { topic = "" } = useParams();
  const nav = useNavigate();
  const metrics = useLoad((s) => queueTopicMetrics(topic, s), `topic-metrics:${topic}`);
  const subs = useLoad((s) => listQueueSubscriptions(topic, s), `topic-subs:${topic}`);
  const [error, setError] = useState<unknown>();
  const [needForce, setNeedForce] = useState(false);

  async function remove(force: boolean) {
    setError(undefined);
    try {
      await deleteQueueTopic(topic, force);
      nav("/queues");
    } catch (e) {
      if (e instanceof ApiError && e.code === "topic_has_subscriptions") setNeedForce(true);
      setError(e);
    }
  }

  const m = metrics.data;
  return (
    <div className="space-y-4">
      <Link to="/queues" className="text-sm text-primary hover:underline">← All topics</Link>
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h2 className="font-mono text-lg">{topic}</h2>
        <div className="flex gap-2">
          <Button onClick={() => { metrics.reload(); subs.reload(); }}>Refresh</Button>
          <ConfirmButton label="Delete topic" onConfirm={() => void remove(false)} />
          {needForce && <ConfirmButton label="Delete topic and its subscriptions" onConfirm={() => void remove(true)} />}
        </div>
      </div>
      <ErrorView error={error ?? metrics.error} />
      {metrics.loading && !metrics.error && <Loading what="Loading topic" />}
      {m && (
        <Panel title="Metrics">
          <dl aria-label="Topic metrics" className="grid gap-x-6 gap-y-1 text-sm md:grid-cols-3">
            {([
              ["Retained messages", m.messages], ["Retained size", bytes(m.bytes)], ["Oldest message", m.oldest_message_age_ms == null ? "none" : `${duration(m.oldest_message_age_ms)} old`],
              ["Published in total", m.published_total], ["Last minute", m.published_last_minute], ["Rate", `${m.publish_rate_per_sec.toFixed(2)}/s`], ["Subscriptions", m.subscriptions],
            ] as [string, string | number][]).map(([k, v]) => <div key={k} className="flex gap-2"><dt className="w-36 shrink-0 text-muted-foreground">{k}</dt><dd>{v}</dd></div>)}
          </dl>
        </Panel>
      )}
      <PublishPanel topic={topic} onDone={() => metrics.reload()} />
      <Messages topic={topic} version={m?.published_total ?? 0} />
      <Subscriptions topic={topic} subs={subs} />
    </div>
  );
}

function PublishPanel({ topic, onDone }: { topic: string; onDone: () => void }) {
  const [data, setData] = useState('{"hello":"world"}');
  const [attrs, setAttrs] = useState("");
  const [key, setKey] = useState("");
  const [dedup, setDedup] = useState("");
  const [error, setError] = useState<unknown>();
  const [last, setLast] = useState("");

  async function submit(e: React.FormEvent) {
    e.preventDefault();
    setError(undefined);
    setLast("");
    const parsed = parseJson(data);
    if (!parsed.ok) return setError(new Error("The message data is not valid JSON."));
    try {
      const [r] = await publishQueueMessages(topic, [{ data: parsed.value, attributes: parseAttrs(attrs), ...(key ? { ordering_key: key } : {}), ...(dedup ? { dedup_id: dedup } : {}) }]);
      setLast(r ? `Published ${r.message_id} (sequence ${r.seq})${r.duplicate ? ", a duplicate: accepted once before" : ""}.` : "");
      onDone();
    } catch (err) {
      setError(err);
    }
  }
  return (
    <Panel title="Publish">
      <form onSubmit={(e) => void submit(e)} className="space-y-3 text-sm">
        <label className="block">Data (JSON)<textarea className={`${inputCls} mt-1 block h-24 w-full font-mono text-xs`} value={data} onChange={(e) => setData(e.target.value)} /></label>
        <label className="block">Attributes (one key=value per line)<textarea className={`${inputCls} mt-1 block h-16 w-full font-mono text-xs`} value={attrs} onChange={(e) => setAttrs(e.target.value)} /></label>
        <div className="flex flex-wrap items-end gap-3">
          <label>Ordering key<Input size="sm" className="ml-2 w-40" value={key} onChange={(e) => setKey(e.target.value)} /></label>
          <label>Dedup id<Input size="sm" className="ml-2 w-40" value={dedup} onChange={(e) => setDedup(e.target.value)} /></label>
          <Button type="submit" tone="primary">Publish message</Button>
        </div>
        {last && <p role="status" className="text-success">{last}</p>}
        <ErrorView error={error} />
      </form>
    </Panel>
  );
}

function Messages({ topic, version }: { topic: string; version: number }) {
  const [rows, setRows] = useState<QueueMessage[]>([]);
  const [next, setNext] = useState<number | null | undefined>(undefined);
  const [error, setError] = useState<unknown>();
  const [loading, setLoading] = useState(true);
  const [open, setOpen] = useState<string>();

  // Reload from the start whenever the topic changes or something was published.
  useEffect(() => {
    let live = true;
    queueTopicMessages(topic, 0, PAGE)
      .then((p) => { if (live) { setRows(p.items); setNext(p.next_after); setError(undefined); } })
      .catch((e: unknown) => { if (live) setError(e); })
      .finally(() => { if (live) setLoading(false); });
    return () => { live = false; };
  }, [topic, version]);

  async function more() {
    if (!next) return;
    try {
      const p = await queueTopicMessages(topic, next, PAGE);
      setRows((r) => [...r, ...p.items]);
      setNext(p.next_after);
    } catch (e) {
      setError(e);
    }
  }

  return (
    <Panel title="Messages">
      <ErrorView error={error} />
      {loading && <Loading what="Loading messages" />}
      {!loading && rows.length === 0 && !error && <p className="text-sm text-muted-foreground">No retained messages.</p>}
      {rows.length > 0 && (
        <Table aria-label="Messages" className="w-full text-left text-sm">
          <Table.Header className="text-xs uppercase tracking-wide text-muted-foreground">
            <Table.Row><Table.Head className="py-1">Seq</Table.Head><Table.Head>Message</Table.Head><Table.Head>Published</Table.Head><Table.Head>Key</Table.Head><Table.Head>Size</Table.Head><Table.Head /></Table.Row>
          </Table.Header>
          <Table.Body>
            {rows.map((r) => (
              <Fragment key={r.id}>
                <Table.Row className="border-t border-border">
                  <Table.Cell className="py-1.5">{r.seq}</Table.Cell><Table.Cell className="font-mono text-xs">{r.id}</Table.Cell><Table.Cell className="text-muted-foreground">{when(r.published_at)}</Table.Cell>
                  <Table.Cell>{r.ordering_key ?? ""}</Table.Cell><Table.Cell>{bytes(r.bytes)}</Table.Cell>
                  <Table.Cell><button type="button" className="text-primary hover:underline" aria-label={`${open === r.id ? "Hide" : "Show"} message ${r.seq}`} onClick={() => setOpen(open === r.id ? undefined : r.id)}>{open === r.id ? "Hide" : "Show"}</button></Table.Cell>
                </Table.Row>
                {open === r.id && (
                  <Table.Row>
                    <Table.Cell colSpan={6} className="pb-2">
                      <pre aria-label={`Message ${r.seq} data`} className="overflow-x-auto rounded bg-background p-2 font-mono text-xs">{pretty(r.data)}</pre>
                      {Object.keys(r.attributes).length > 0 && <p className="mt-1 text-xs text-muted-foreground">Attributes: {Object.entries(r.attributes).map(([k, v]) => `${k}=${v}`).join(", ")}</p>}
                    </Table.Cell>
                  </Table.Row>
                )}
              </Fragment>
            ))}
          </Table.Body>
        </Table>
      )}
      {next ? <div className="mt-3"><Button onClick={() => void more()}>Load more</Button></div> : null}
    </Panel>
  );
}

function Subscriptions({ topic, subs }: { topic: string; subs: LoadState<QueueSubscription[]> }) {
  const [name, setName] = useState("");
  const [deadline, setDeadline] = useState("30");
  const [attempts, setAttempts] = useState("");
  const [dlt, setDlt] = useState("");
  const [ordering, setOrdering] = useState(false);
  const [start, setStart] = useState<"latest" | "earliest">("latest");
  const [error, setError] = useState<unknown>();

  async function create(e: React.FormEvent) {
    e.preventDefault();
    setError(undefined);
    try {
      await createQueueSubscription({
        name: name.trim(), topic, ack_deadline_ms: Math.round(Number(deadline) * 1000), ordering, start,
        ...(attempts ? { max_delivery_attempts: Number(attempts) } : {}), ...(dlt ? { dead_letter_topic: dlt.trim() } : {}),
      });
      setName("");
      subs.reload();
    } catch (err) {
      setError(err);
    }
  }
  return (
    <Panel title="Subscriptions of this topic">
      <ErrorView error={subs.error} />
      {subs.data && subs.data.length === 0 && <p className="mb-3 text-sm text-muted-foreground">No subscriptions: messages are kept for the retention period but delivered to nobody.</p>}
      {subs.data && subs.data.length > 0 && (
        <ul aria-label="Topic subscriptions" className="mb-3 space-y-1 text-sm">
          {subs.data.map((s) => <li key={s.name}><Link className="text-primary hover:underline" to={`/queues/s/${encodeURIComponent(s.name)}`}>{s.name}</Link> <span className="text-muted-foreground">ack deadline {duration(s.ack_deadline_ms)}{s.ordering ? ", ordered" : ""}</span></li>)}
        </ul>
      )}
      <form onSubmit={(e) => void create(e)} className="flex flex-wrap items-end gap-3 text-sm">
        <label>Subscription name<Input size="sm" className="ml-2 w-40" value={name} onChange={(e) => setName(e.target.value)} /></label>
        <label>Ack deadline (s)<Input size="sm" className="ml-2 w-16" inputMode="numeric" value={deadline} onChange={(e) => setDeadline(e.target.value)} /></label>
        <label>Max attempts<Input size="sm" className="ml-2 w-16" inputMode="numeric" value={attempts} onChange={(e) => setAttempts(e.target.value)} /></label>
        <label>Dead-letter topic<Input size="sm" className="ml-2 w-36" value={dlt} onChange={(e) => setDlt(e.target.value)} /></label>
        <label>Start<select className={`${inputCls} ml-2`} value={start} onChange={(e) => setStart(e.target.value as "latest" | "earliest")}><option value="latest">latest</option><option value="earliest">earliest</option></select></label>
        <label className="flex items-center gap-1"><input type="checkbox" checked={ordering} onChange={(e) => setOrdering(e.target.checked)} />Ordered</label>
        <Button type="submit" tone="primary" disabled={!name.trim()}>Create subscription</Button>
      </form>
      <div className="mt-3"><ErrorView error={error} /></div>
    </Panel>
  );
}
