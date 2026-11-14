import { Fragment, useEffect, useId, useState } from "react";
import { Link, useNavigate, useParams } from "react-router-dom";
import { ApiError, createQueueSubscription, deleteQueueTopic, getQueueTopic, listQueueSubscriptions, publishQueueMessages, queueTopicMessages, queueTopicMetrics } from "../../api";
import type { QueueMessage, QueueSubscription } from "../../api";
import { Button, ErrorView } from "../../components/ui";
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
import TopicCatalogEntry from "./TopicCatalogEntry";
import { bytes, duration, parseAttrs, parseJson, pretty } from "./util";
import { CardBody, HEAD_ROW, TableCard } from "../../components/TableCard";
import { Code, CopyId, NUM, Section, StatGrid, When } from "../jobs/ops";
import { ArrowDown01Icon } from "@hugeicons/core-free-icons";
import { HugeiconsIcon } from "@hugeicons/react";
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
        description={<span className="inline-flex flex-wrap items-center gap-x-2 gap-y-1">Topic · tenant <Tenant value={t?.tenant} />{t?.created_by ? <> · created by <span className="font-mono text-xs">{t.created_by}</span></> : null}{t ? <> · retention {duration(t.retention_ms)}</> : null}</span>}
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
      {metrics.loading && !metrics.data && !metrics.error && <SkeletonLines lines={3} label="Loading topic" />}
      {t?.source && (
        <p role="note" className="rounded-lg border border-info/30 bg-info/5 p-3 text-sm text-foreground/80">
          This topic carries the record changes of <span className="font-mono">{t.source.collection}</span> ({t.source.ops.join(", ")}). A reader gets each record as <em>its subscription&apos;s owner</em> may see it: hidden fields are removed, and a record the owner may not see is withheld, so it never appears for that subscription. The messages listed below show your own view.
        </p>
      )}
      {m && (
        <StatGrid
          label="Topic metrics"
          items={[
            { label: "Retained messages", value: m.messages.toLocaleString() },
            { label: "Retained size", value: bytes(m.bytes) },
            { label: "Oldest message", value: m.oldest_message_age_ms == null ? "none" : `${duration(m.oldest_message_age_ms)} old` },
            { label: "Published in total", value: m.published_total.toLocaleString() },
            { label: "Last minute", value: m.published_last_minute.toLocaleString() },
            { label: "Rate", value: `${m.publish_rate_per_sec.toFixed(2)}/s` },
            { label: "Subscriptions", value: m.subscriptions, tone: m.subscriptions === 0 ? "warn" : undefined },
          ]}
        />
      )}
      <Messages topic={topic} version={m?.published_total ?? 0} />
      <Subscriptions subs={subs} />
      {t && <TopicCatalogEntry key={`${t.name}:${t.updated_at}`} topic={t} onSaved={info.reload} />}
      <PublishPanel topic={topic} onDone={() => metrics.reload()} />
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
  const [busy, setBusy] = useState(false);
  const id = useId();

  async function submit(e: React.FormEvent) {
    e.preventDefault();
    setError(undefined);
    setLast("");
    const parsed = parseJson(data);
    if (!parsed.ok) return setError(new Error("The message data is not valid JSON."));
    setBusy(true);
    try {
      const [r] = await publishQueueMessages(topic, [{ data: parsed.value, attributes: parseAttrs(attrs), ...(key ? { ordering_key: key } : {}), ...(dedup ? { dedup_id: dedup } : {}) }]);
      setLast(r ? `Published ${r.message_id} (sequence ${r.seq})${r.duplicate ? ", a duplicate: accepted once before" : ""}.` : "");
      onDone();
    } catch (err) {
      setError(err);
    } finally {
      setBusy(false);
    }
  }
  const lbl = "mb-1.5 flex items-center gap-1 text-sm font-medium";
  return (
    <Section title="Publish a message" description="Send a test message to this topic. Your app publishes the same way with the SDK (see Use from code).">
      <form aria-label="Publish" onSubmit={(e) => void submit(e)} className="grid gap-4 text-sm lg:grid-cols-3">
        <div className="lg:col-span-2">
          <label htmlFor={`${id}data`} className={lbl}>Data (JSON)</label>
          <Textarea id={`${id}data`} size="sm" autoResize={false} spellCheck={false} className="block h-28 w-full font-mono text-xs" value={data} onChange={(e) => setData(e.target.value)} />
        </div>
        <div>
          <label htmlFor={`${id}attrs`} className={lbl}>Attributes</label>
          <Textarea id={`${id}attrs`} size="sm" autoResize={false} spellCheck={false} placeholder={"source=checkout\nregion=eu"} aria-describedby={`${id}attrs-help`} className="block h-28 w-full font-mono text-xs" value={attrs} onChange={(e) => setAttrs(e.target.value)} />
          <p id={`${id}attrs-help`} className="mt-1 text-xs text-muted-foreground">One key=value per line. Optional.</p>
        </div>
        <div className="flex flex-wrap items-end gap-3 lg:col-span-3">
          <div>
            <span className={lbl}><label htmlFor={`${id}key`}>Ordering key</label><HelpTip term="ordering_key" /></span>
            <Input id={`${id}key`} size="sm" className="w-48" value={key} onChange={(e) => setKey(e.target.value)} />
          </div>
          <div>
            <span className={lbl}><label htmlFor={`${id}dedup`}>Dedup id</label><HelpTip term="dedup_id" /></span>
            <Input id={`${id}dedup`} size="sm" className="w-48" value={dedup} onChange={(e) => setDedup(e.target.value)} />
          </div>
          <Button type="submit" tone="primary" disabled={busy}>{busy ? "Publishing…" : "Publish message"}</Button>
          {last && <p role="status" className="text-sm text-success">{last}</p>}
        </div>
        <div className="lg:col-span-3"><ErrorView error={error} /></div>
      </form>
    </Section>
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
    <TableCard
      title="Messages"
      count={rows.length > 0 ? `${rows.length}${next ? "+" : ""}` : undefined}
      description="Retained messages, oldest first. Open one to see its data and attributes."
      footer={rows.length > 0 ? <Pager mode="more" shown={rows.length} hasMore={!!next} loading={more} onMore={() => void loadMore()} noun="messages" label="More messages" /> : undefined}
    >
      <CardBody className="space-y-3">
        <ErrorView error={error} />
        {loading && <SkeletonTable rows={3} columns={6} label="Loading messages" />}
        {!loading && rows.length === 0 && !error && <EmptyState compact title="No retained messages" why="Publish one below, or publish from your code (Use from code). Messages are kept for the topic's retention period." />}
      </CardBody>
      {rows.length > 0 && (
          <Table framed density="compact" aria-label="Messages">
            <Table.Header className={HEAD_ROW}>
              <Table.Row><Table.Head className={`py-1 ${NUM}`}>Seq</Table.Head><Table.Head>Message</Table.Head><Table.Head>Ordering key</Table.Head><Table.Head className={NUM}>Size</Table.Head><Table.Head className="text-right">Published</Table.Head><Table.Head><span className="sr-only">Data</span></Table.Head></Table.Row>
            </Table.Header>
            <Table.Body>
              {rows.map((r) => (
                <Fragment key={r.id}>
                  <Table.Row className={open === r.id ? "bg-muted/30" : undefined}>
                    <Table.Cell className={`w-16 py-2 ${NUM}`}>{r.seq}</Table.Cell>
                    <Table.Cell className="max-w-72"><CopyId id={r.id} noun="message" /></Table.Cell>
                    <Table.Cell>{r.ordering_key ? <span className="font-mono text-xs">{r.ordering_key}</span> : <span className="text-muted-foreground">—</span>}</Table.Cell>
                    <Table.Cell className={NUM}>{bytes(r.bytes)}</Table.Cell>
                    <Table.Cell className="text-right text-muted-foreground"><When iso={r.published_at} /></Table.Cell>
                    <Table.Cell className="w-20 text-right">
                      <button type="button" aria-expanded={open === r.id} className="inline-flex items-center gap-1 rounded-md px-1.5 py-0.5 text-primary hover:bg-muted" aria-label={`${open === r.id ? "Hide" : "Show"} message ${r.seq}`} onClick={() => setOpen(open === r.id ? undefined : r.id)}>
                        {open === r.id ? "Hide" : "Show"}
                        <HugeiconsIcon icon={ArrowDown01Icon} size={14} strokeWidth={1.8} aria-hidden="true" className={`transition-transform ${open === r.id ? "rotate-180" : ""}`} />
                      </button>
                    </Table.Cell>
                  </Table.Row>
                  {open === r.id && (
                    <Table.Row className="bg-muted/30">
                      <Table.Cell colSpan={6} className="pb-3">
                        <Code label={`Message ${r.seq} data`}>{pretty(r.data)}</Code>
                        {Object.keys(r.attributes).length > 0 && (
                          <p className="mt-2 flex flex-wrap items-center gap-1.5 text-xs text-muted-foreground">Attributes: {Object.entries(r.attributes).map(([k, v]) => <code key={k} className="rounded bg-muted px-1.5 py-0.5 font-mono text-foreground">{`${k}=${v}`}</code>)}</p>
                        )}
                      </Table.Cell>
                    </Table.Row>
                  )}
                </Fragment>
              ))}
            </Table.Body>
          </Table>
      )}
    </TableCard>
  );
}

function Subscriptions({ subs }: { subs: LoadState<QueueSubscription[]> }) {
  return (
    <Section title="Subscriptions of this topic" count={subs.data?.length} description="Each subscription gets its own copy of every message and keeps its own place.">
      <ErrorView error={subs.error} />
      {subs.data && subs.data.length === 0 && <EmptyState compact title="No subscriptions" why="Messages are kept for the retention period but delivered to nobody. Choose New subscription to add a reader." />}
      {subs.data && subs.data.length > 0 && (
        <ul aria-label="Topic subscriptions" className="divide-y divide-border text-sm">
          {subs.data.map((s) => (
            <li key={s.name} className="flex flex-wrap items-center gap-x-3 gap-y-1 py-2.5 first:pt-0 last:pb-0">
              <Link className="font-medium text-primary hover:underline" to={`/queues/s/${encodeURIComponent(s.name)}`}>{s.name}</Link>
              <span className="rounded-md bg-muted px-1.5 py-0.5 text-xs">{s.push ? `push (${s.push.kind})` : "pull"}</span>
              <span className="min-w-0 truncate text-xs text-muted-foreground">ack deadline {duration(s.ack_deadline_ms)}{s.ordering ? ", ordered" : ""} · owner {s.created_by}</span>
            </li>
          ))}
        </ul>
      )}
    </Section>
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
