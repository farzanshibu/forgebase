import { Fragment, useEffect, useState } from "react";
import { Link, useNavigate, useParams } from "react-router-dom";
import { ApiError, createQueueSubscription, deleteQueueTopic, getQueueTopic, listQueueSubscriptions, publishQueueMessages, queueTopicMessages, queueTopicMetrics } from "../../api";
import type { QueueMessage, QueueSubscription } from "../../api";
import { Button, ErrorView, Panel } from "../../components/ui";
import { ConfirmDialog } from "../../components/ConfirmDialog";
import { CreateSheet } from "../../components/CreateSheet";
import { EmptyState } from "../../components/EmptyState";
import { FormField } from "../../components/FormField";
import { HelpTip } from "../../components/HelpTip";
import { PageHeader } from "../../components/PageHeader";
import { Pager } from "../../components/Pager";
import { SkeletonLines, SkeletonTable } from "../../components/Skeleton";
import { UseFromCodeButton } from "../../components/UseFromCode";
import { fieldErrors } from "../../errors";
import { useLoad, type LoadState } from "../../hooks";
import { notify } from "../../toaster";
import { validate } from "../../validate";
import Tenant from "../jobs/Tenant";
import QueueRules from "./QueueRules";
import { when } from "../functions/util";
import { bytes, duration, parseAttrs, parseJson, pretty } from "./util";
import { Table } from "panelui-web/table";
import { Input } from "panelui-web/input";
import { Textarea } from "panelui-web/textarea";
import { Check, Sel } from "../../components/controls";

const PAGE = 25;

export default function TopicDetail() {
  const { topic = "" } = useParams();
  const nav = useNavigate();
  const info = useLoad((s) => getQueueTopic(topic, s), `topic:${topic}`);
  const metrics = useLoad((s) => queueTopicMetrics(topic, s), `topic-metrics:${topic}`);
  const subs = useLoad((s) => listQueueSubscriptions(topic, s), `topic-subs:${topic}`);
  const [error, setError] = useState<unknown>();
  const [needForce, setNeedForce] = useState(false);
  const [adding, setAdding] = useState(false);

  async function remove(force: boolean) {
    setError(undefined);
    try {
      await deleteQueueTopic(topic, force);
      notify.deleted(`Topic ${topic}`);
      nav("/queues");
    } catch (e) {
      if (e instanceof ApiError && e.code === "topic_has_subscriptions") setNeedForce(true);
      setError(e);
      throw e;
    }
  }

  const m = metrics.data;
  const t = info.data;
  return (
    <div className="space-y-4">
      <PageHeader
        title={topic}
        description={<span className="inline-flex flex-wrap items-center gap-2">Topic · tenant <Tenant value={t?.tenant} />{t?.created_by ? <> · created by <span className="font-mono text-xs">{t.created_by}</span></> : null}</span>}
        docs="docs/adr/0022-queues-pubsub.md"
        secondary={<><Button onClick={() => { info.reload(); metrics.reload(); subs.reload(); }}>Refresh</Button><UseFromCodeButton kind="topic" name={topic} /></>}
        primary={
          <span className="flex gap-2">
            <Button tone="primary" onClick={() => setAdding(true)}>New subscription</Button>
            <ConfirmDialog
              trigger={<Button tone="danger">Delete topic</Button>}
              title={`Delete topic ${topic}?`}
              consequence={needForce ? "It still has subscriptions: they are deleted with it, with their dead letters and snapshots." : "Every retained message is deleted. Publishers get 404."}
              confirmLabel={needForce ? "Delete topic and its subscriptions" : "Delete topic"}
              confirmText={topic}
              onConfirm={() => remove(needForce)}
            />
          </span>
        }
      />
      <ErrorView error={error ?? metrics.error ?? info.error} />
      {metrics.loading && !metrics.error && <SkeletonLines lines={3} label="Loading topic" />}
      {t?.source && (
        <p role="note" className="rounded-md border border-border p-3 text-sm text-muted-foreground">
          This topic carries the record changes of <span className="font-mono">{t.source.collection}</span> ({t.source.ops.join(", ")}). A reader gets each record as <em>its subscription&apos;s owner</em> may see it: hidden fields are removed, and a record the owner may not see is withheld, so it never appears for that subscription. The messages listed below show your own view.
        </p>
      )}
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
      <NewSubscription topic={topic} tenant={t?.tenant ?? undefined} open={adding} onOpenChange={setAdding} onCreated={subs.reload} />
      <QueueRules kind="queue_topic" name={topic} />
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
      <form aria-label="Publish" onSubmit={(e) => void submit(e)} className="space-y-3 text-sm">
        <label className="block">Data (JSON)<Textarea size="sm" autoResize={false} className="mt-1 block h-24 w-full font-mono text-xs" value={data} onChange={(e) => setData(e.target.value)} /></label>
        <label className="block">Attributes (one key=value per line)<Textarea size="sm" autoResize={false} className="mt-1 block h-16 w-full font-mono text-xs" value={attrs} onChange={(e) => setAttrs(e.target.value)} /></label>
        <div className="flex flex-wrap items-end gap-3">
          <label className="inline-flex items-center gap-1">Ordering key <HelpTip term="ordering_key" /><Input size="sm" className="ml-1 w-40" value={key} onChange={(e) => setKey(e.target.value)} /></label>
          <label className="inline-flex items-center gap-1">Dedup id <HelpTip term="dedup_id" /><Input size="sm" className="ml-1 w-40" value={dedup} onChange={(e) => setDedup(e.target.value)} /></label>
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
  const [more, setMore] = useState(false);
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

  async function loadMore() {
    if (!next) return;
    setMore(true);
    try {
      const p = await queueTopicMessages(topic, next, PAGE);
      setRows((r) => [...r, ...p.items]);
      setNext(p.next_after);
    } catch (e) {
      setError(e);
    } finally {
      setMore(false);
    }
  }

  return (
    <Panel title="Messages">
      <ErrorView error={error} />
      {loading && <SkeletonTable rows={3} columns={6} label="Loading messages" />}
      {!loading && rows.length === 0 && !error && <EmptyState compact title="No retained messages" why="Publish one above, or publish from your code (Use from code). Messages are kept for the topic's retention period." />}
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
      {rows.length > 0 && <Pager mode="more" shown={rows.length} hasMore={!!next} loading={more} onMore={() => void loadMore()} noun="messages" label="More messages" />}
    </Panel>
  );
}

function Subscriptions({ topic, subs }: { topic: string; subs: LoadState<QueueSubscription[]> }) {
  return (
    <Panel title="Subscriptions of this topic">
      <ErrorView error={subs.error} />
      {subs.data && subs.data.length === 0 && <EmptyState compact title="No subscriptions" why="Messages are kept for the retention period but delivered to nobody. Choose New subscription to add a reader." />}
      {subs.data && subs.data.length > 0 && (
        <ul aria-label="Topic subscriptions" className="space-y-1 text-sm">
          {subs.data.map((s) => (
            <li key={s.name}>
              <Link className="text-primary hover:underline" to={`/queues/s/${encodeURIComponent(s.name)}`}>{s.name}</Link>{" "}
              <span className="text-muted-foreground">ack deadline {duration(s.ack_deadline_ms)}{s.ordering ? ", ordered" : ""} · owner {s.created_by}</span>
            </li>
          ))}
        </ul>
      )}
      <span hidden>{topic}</span>
    </Panel>
  );
}

/** New subscription for this topic. The owner is the signed-in account: record messages reach the subscription as that account sees them. */
function NewSubscription({ topic, tenant: topicTenant, open, onOpenChange, onCreated }: { topic: string; tenant?: string; open: boolean; onOpenChange: (open: boolean) => void; onCreated: () => void }) {
  const [name, setName] = useState("");
  const [deadline, setDeadline] = useState("30");
  const [attempts, setAttempts] = useState("");
  const [dlt, setDlt] = useState("");
  const [ordering, setOrdering] = useState(false);
  const [start, setStart] = useState<"latest" | "earliest">("latest");
  const [tenant, setTenant] = useState("");
  const [touched, setTouched] = useState(false);
  const [saveError, setSaveError] = useState<unknown>();
  const n = name.trim();
  const nameError = (touched ? validate.topicName(n) : undefined) ?? fieldErrors(saveError).name;
  const secs = (v: string) => (v && !(Number(v) > 0) ? "Use a number above 0." : undefined);
  return (
    <CreateSheet
      open={open} onOpenChange={onOpenChange}
      title="New subscription" description={`A subscription reads topic ${topic} with its own cursor. You become its owner.`}
      submitLabel="Create subscription" canSubmit={!validate.topicName(n) && !secs(deadline)} onError={setSaveError}
      onSubmit={async () => {
        const t = tenant.trim() || topicTenant;
        const made = await createQueueSubscription({
          name: n, topic, ack_deadline_ms: Math.round(Number(deadline || "30") * 1000), ordering, start,
          ...(attempts ? { max_delivery_attempts: Number(attempts) } : {}), ...(dlt ? { dead_letter_topic: dlt.trim() } : {}),
        }, t || undefined);
        setName(""); setAttempts(""); setDlt(""); setTouched(false); setSaveError(undefined);
        onCreated();
        return { what: `Subscription ${made.name ?? n}`, href: `/queues/s/${encodeURIComponent(made.name ?? (t ? `${t}/${n}` : n))}` };
      }}
    >
      <FormField label="Name" required help="Letters, digits, dots, dashes, underscores or colons." error={nameError}>
        {(p) => <Input {...p} size="sm" className="w-full" value={name} onChange={(e) => { setName(e.target.value); setTouched(true); }} />}
      </FormField>
      <FormField label="Ack deadline (seconds)" term="ack_deadline" error={secs(deadline)}>
        {(p) => <Input {...p} size="sm" className="w-full" inputMode="decimal" value={deadline} onChange={(e) => setDeadline(e.target.value)} />}
      </FormField>
      <FormField label="Max attempts" help="After this many failed deliveries a message goes to the dead-letter topic. Empty: unlimited.">
        {(p) => <Input {...p} size="sm" className="w-full" inputMode="numeric" value={attempts} onChange={(e) => setAttempts(e.target.value)} />}
      </FormField>
      <FormField label="Dead-letter topic" term="dead_letter" help="Optional. You need permission to publish to it.">
        {(p) => <Input {...p} size="sm" className="w-full" value={dlt} onChange={(e) => setDlt(e.target.value)} />}
      </FormField>
      <FormField label="Start" help="Latest: only new messages. Earliest: every message still retained.">
        {(p) => <Sel {...p} aria-label="Start" value={start} onChange={(v) => setStart(v as "latest" | "earliest")} options={[{ value: "latest", label: "latest" }, { value: "earliest", label: "earliest" }]} />}
      </FormField>
      <FormField label="Tenant" help="Optional. Defaults to the topic's tenant.">
        {(p) => <Input {...p} size="sm" className="w-full" value={tenant} onChange={(e) => setTenant(e.target.value)} placeholder={topicTenant ?? ""} />}
      </FormField>
      <Check checked={ordering} onChange={setOrdering} label="Ordered" description="Deliver messages with the same ordering key one after another." />
    </CreateSheet>
  );
}
