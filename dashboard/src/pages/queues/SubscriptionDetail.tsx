import { useState } from "react";
import { useNavigate, useParams } from "react-router-dom";
import { createQueueSnapshot, queueDeadLetters, deleteQueueSnapshot, deleteQueueSubscription, getQueueSubscription, listQueueSnapshots, seekQueueSubscription, queueSubscriptionMetrics } from "../../api";
import { Button, ErrorView, Panel } from "../../components/ui";
import { ConfirmDialog } from "../../components/ConfirmDialog";
import { EmptyState } from "../../components/EmptyState";
import { HelpTip } from "../../components/HelpTip";
import { PageHeader } from "../../components/PageHeader";
import { SkeletonLines } from "../../components/Skeleton";
import { UseFromCodeButton } from "../../components/UseFromCode";
import { notify } from "../../toaster";
import Tenant from "../jobs/Tenant";
import QueueRules from "./QueueRules";
import { useLoad } from "../../hooks";
import { when } from "../functions/util";
import { duration, pretty } from "./util";
import { Input } from "panelui-web/input";
import { Sel } from "../../components/controls";

export default function SubscriptionDetail() {
  const { sub = "" } = useParams();
  const nav = useNavigate();
  const info = useLoad((s) => getQueueSubscription(sub, s), `sub:${sub}`);
  const metrics = useLoad((s) => queueSubscriptionMetrics(sub, s), `sub-metrics:${sub}`);
  const dead = useLoad((s) => queueDeadLetters(sub, undefined, 25, s), `sub-dead:${sub}`);
  const snaps = useLoad((s) => listQueueSnapshots(s), "queue-snapshots");
  const [error, setError] = useState<unknown>();
  const [notice, setNotice] = useState("");
  const [time, setTime] = useState("");
  const [snapName, setSnapName] = useState("");
  const [snapPick, setSnapPick] = useState("");
  const [open, setOpen] = useState<number>();

  const refresh = () => { info.reload(); metrics.reload(); dead.reload(); snaps.reload(); };
  async function run(fn: () => Promise<unknown>, announce: () => void) {
    setError(undefined);
    setNotice("");
    try {
      await fn();
      announce();
      refresh();
    } catch (e) {
      setError(e);
    }
  }
  async function replay(to: { time: string } | { snapshot: string }) {
    setError(undefined);
    setNotice("");
    try {
      const r = await seekQueueSubscription(sub, to);
      setNotice(`Replay set: the cursor is at sequence ${r.cursor_seq} and ${r.restored} message(s) were put back.`);
      refresh();
    } catch (e) {
      setError(e);
    }
  }

  const s = info.data;
  const m = metrics.data;
  const mine = (snaps.data ?? []).filter((x) => x.subscription === sub || x.topic === s?.topic);
  return (
    <div className="space-y-4">
      <PageHeader
        title={sub}
        description={<span className="inline-flex flex-wrap items-center gap-2">Subscription of <span className="font-mono">{s?.topic ?? "a topic"}</span> · tenant <Tenant value={s?.tenant} /> · owner <span className="font-mono text-xs">{s?.created_by ?? "unknown"}</span></span>}
        docs="docs/adr/0022-queues-pubsub.md"
        secondary={<><Button onClick={refresh}>Refresh</Button><UseFromCodeButton kind="subscription" name={sub} /></>}
        primary={
          <ConfirmDialog
            trigger={<Button tone="danger">Delete subscription</Button>}
            title={`Delete subscription ${sub}?`}
            consequence="Its cursor, dead letters and snapshots are deleted. Messages stay in the topic."
            confirmLabel="Delete subscription" confirmText={sub}
            onConfirm={async () => { await deleteQueueSubscription(sub); notify.deleted(`Subscription ${sub}`); nav("/queues/subscriptions"); }}
          />
        }
      />
      <ErrorView error={error ?? info.error} />
      {notice && <p role="status" className="text-sm text-success">{notice}</p>}
      {info.loading && !info.error && <SkeletonLines lines={4} label="Loading subscription" />}
      {s && (
        <Panel title="Settings">
          <p className="mb-3 text-xs text-muted-foreground">
            This subscription delivers as its owner, <span className="font-mono">{s.created_by}</span>. Record changes reach it as that account may see them: hidden fields are removed, and records it may not see are withheld, so they never appear here.
          </p>
          <dl aria-label="Subscription settings" className="grid gap-x-6 gap-y-1 text-sm md:grid-cols-2">
            {([
              ["Topic", s.topic], ["Owner", s.created_by], ["Tenant", s.tenant ?? "instance"], ["Delivery", s.push ? `push to ${s.push.kind} ${s.push.endpoint}` : "pull"], ["Ack deadline", duration(s.ack_deadline_ms)],
              ["Max attempts", s.max_delivery_attempts ?? "unlimited"], ["Backoff", `${duration(s.backoff_base_ms)} up to ${duration(s.backoff_max_ms)}`],
              ["Dead-letter topic", s.dead_letter_topic ?? "none"], ["Ordering", s.ordering ? "per ordering key" : "off"], ["Filter", s.filter ? JSON.stringify(s.filter) : "none"],
            ] as [string, string | number][]).map(([k, v]) => <div key={k} className="flex gap-2"><dt className="w-36 shrink-0 text-muted-foreground">{k}</dt><dd>{v}</dd></div>)}
          </dl>
        </Panel>
      )}
      {m && (
        <Panel title="Backlog">
          <dl aria-label="Subscription metrics" className="grid gap-x-6 gap-y-1 text-sm md:grid-cols-3">
            {([
              ["Backlog", `${m.backlog_messages} message(s)`], ["Ready", m.ready], ["Delayed (backoff)", m.delayed], ["In flight", m.in_flight],
              ["Oldest unacked", m.oldest_unacked_age_ms == null ? "none" : duration(m.oldest_unacked_age_ms)], ["Ack rate", `${m.ack_rate_per_sec.toFixed(2)}/s`],
              ["Delivered", m.delivered_total], ["Acked", m.acked_total], ["Nacked", m.nacked_total], ["Expired", m.expired_total], ["Dead-lettered", m.dead_lettered_total], ["Dead letters kept", m.dead_letters],
            ] as [string, string | number][]).map(([k, v]) => <div key={k} className="flex gap-2"><dt className="w-36 shrink-0 text-muted-foreground">{k}</dt><dd>{v}</dd></div>)}
          </dl>
        </Panel>
      )}
      <Panel title="Dead letters">
        <p className="mb-2 flex items-center gap-1 text-xs text-muted-foreground">What is a dead letter? <HelpTip term="dead_letter" /></p>
        <ErrorView error={dead.error} />
        {dead.data && dead.data.length === 0 && <EmptyState compact title="No dead letters" why="A message lands here after it fails too many times, so you can look at it instead of losing it." />}
        {dead.data && dead.data.length > 0 && (
          <ul aria-label="Dead letters" className="space-y-2 text-sm">
            {dead.data.map((d) => (
              <li key={d.id} className="rounded border border-border p-2">
                <div className="flex flex-wrap items-center gap-x-3">
                  <span className="font-mono text-xs">{d.message_id}</span><span className="text-muted-foreground">{when(d.at)}</span>
                  <span>{d.attempts} attempt(s)</span><span className="text-destructive">{d.reason}</span>
                  {d.dead_letter_topic && <span className="text-muted-foreground">forwarded to {d.dead_letter_topic}</span>}
                  <button type="button" className="text-primary hover:underline" aria-label={`${open === d.id ? "Hide" : "Show"} dead letter ${d.id}`} onClick={() => setOpen(open === d.id ? undefined : d.id)}>{open === d.id ? "Hide" : "Show"}</button>
                </div>
                {open === d.id && (
                  <div className="mt-2 space-y-2">
                    <pre aria-label={`Dead letter ${d.id} data`} className="overflow-x-auto rounded bg-background p-2 font-mono text-xs">{pretty(d.data)}</pre>
                    <ol aria-label={`Dead letter ${d.id} history`} className="space-y-0.5 text-xs text-muted-foreground">
                      {d.history.map((h) => <li key={h.attempt}>attempt {h.attempt} at {when(h.at)}: {h.outcome ?? "?"}{h.error ? ` (${h.error})` : ""}{h.consumer ? ` by ${h.consumer}` : ""}</li>)}
                    </ol>
                  </div>
                )}
              </li>
            ))}
          </ul>
        )}
      </Panel>
      <Panel title="Replay">
        <p className="mb-3 text-sm text-muted-foreground">Seeking puts messages back so they are delivered again, as far back as the topic's retention reaches.</p>
        <form onSubmit={(e) => { e.preventDefault(); void replay({ time: new Date(time).toISOString() }); }} className="flex flex-wrap items-end gap-3 text-sm">
          <label>Replay from<Input size="sm" type="datetime-local" className="ml-2" value={time} onChange={(e) => setTime(e.target.value)} /></label>
          <Button type="submit" disabled={!time || Number.isNaN(new Date(time).getTime())}>Replay from this time</Button>
        </form>
        <form onSubmit={(e) => { e.preventDefault(); void replay({ snapshot: snapPick }); }} className="mt-3 flex flex-wrap items-end gap-3 text-sm">
          <label>Snapshot
            <Sel aria-label="Snapshot" className="ml-2" value={snapPick} onChange={setSnapPick} options={[{ value: "", label: "choose…" }, ...mine.map((x) => ({ value: x.name, label: `${x.name} (cursor ${x.cursor_seq}, ${x.unacked} unacked)` }))]} />
          </label>
          <Button type="submit" disabled={!snapPick}>Restore snapshot</Button>
          {snapPick && <ConfirmDialog trigger={<Button tone="danger" type="button">Delete snapshot</Button>} title={`Delete snapshot ${snapPick}?`} consequence="You can no longer replay from it." confirmLabel="Delete snapshot" confirmText={snapPick} onConfirm={() => run(async () => { await deleteQueueSnapshot(snapPick); setSnapPick(""); }, () => notify.deleted(`Snapshot ${snapPick}`))} />}
        </form>
        <form onSubmit={(e) => { e.preventDefault(); void run(() => createQueueSnapshot(snapName.trim(), sub, s?.tenant ?? undefined), () => notify.created(`Snapshot ${snapName.trim()}`)).then(() => setSnapName("")); }} className="mt-3 flex flex-wrap items-end gap-3 text-sm">
          <label>New snapshot<Input size="sm" className="ml-2 w-40" value={snapName} onChange={(e) => setSnapName(e.target.value)} /></label>
          <Button type="submit" disabled={!snapName.trim()}>Save snapshot</Button>
        </form>
      </Panel>
      <QueueRules kind="queue_subscription" name={sub} />
    </div>
  );
}
