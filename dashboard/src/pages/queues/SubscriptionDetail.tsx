import { useId, useState } from "react";
import { Link, useNavigate, useParams } from "react-router-dom";
import { createQueueSnapshot, queueDeadLetters, deleteQueueSnapshot, deleteQueueSubscription, getQueueSubscription, listQueueSnapshots, seekQueueSubscription, queueSubscriptionMetrics } from "../../api";
import { Button, ErrorView } from "../../components/ui";
import { ConfirmDialog } from "../../components/ConfirmDialog";
import { PageHeader } from "../../components/PageHeader";
import { SkeletonLines } from "../../components/Skeleton";
import { UseFromCodeButton } from "../../components/UseFromCode";
import { notify } from "../../toaster";
import Tenant from "../jobs/Tenant";
import AttemptLog from "./AttemptLog";
import DeadLetters from "./DeadLetters";
import PushTools from "./PushTools";
import QueueRules from "./QueueRules";
import { useLoad } from "../../hooks";
import { duration } from "./util";
import { Facts, Section, StatGrid } from "../jobs/ops";
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
  const uid = useId();

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
  const lbl = "mb-1.5 block text-sm font-medium";
  return (
    <div className="space-y-4">
      <PageHeader
        title={sub}
        description={<span className="inline-flex flex-wrap items-center gap-x-2 gap-y-1">Subscription of {s ? <Link className="font-mono text-primary hover:underline" to={`/queues/t/${encodeURIComponent(s.topic)}`}>{s.topic}</Link> : <span className="font-mono">a topic</span>} · tenant <Tenant value={s?.tenant} /> · owner <span className="font-mono text-xs">{s?.created_by ?? "unknown"}</span></span>}
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
      {notice && <p role="status" className="rounded-lg border border-success/40 bg-success/10 p-3 text-sm text-success">{notice}</p>}
      {info.loading && !info.data && !info.error && <SkeletonLines lines={4} label="Loading subscription" />}
      {m && (
        <StatGrid
          label="Subscription metrics"
          columns={6}
          items={[
            { label: "Backlog", value: `${m.backlog_messages} message(s)`, tone: m.backlog_messages > 0 ? "info" : "ok" },
            { label: "Ready", value: m.ready },
            { label: "In flight", value: m.in_flight },
            { label: "Delayed (backoff)", value: m.delayed, tone: m.delayed > 0 ? "warn" : undefined },
            { label: "Oldest unacked", value: m.oldest_unacked_age_ms == null ? "none" : duration(m.oldest_unacked_age_ms) },
            { label: "Ack rate", value: `${m.ack_rate_per_sec.toFixed(2)}/s` },
            { label: "Delivered", value: m.delivered_total.toLocaleString() },
            { label: "Acked", value: m.acked_total.toLocaleString() },
            { label: "Nacked", value: m.nacked_total.toLocaleString(), tone: m.nacked_total > 0 ? "warn" : undefined },
            { label: "Expired", value: m.expired_total.toLocaleString(), tone: m.expired_total > 0 ? "warn" : undefined },
            { label: "Dead-lettered", value: m.dead_lettered_total.toLocaleString(), tone: m.dead_lettered_total > 0 ? "bad" : undefined },
            { label: "Dead letters kept", value: m.dead_letters.toLocaleString() },
          ]}
        />
      )}
      {s && (
        <Section title="Settings" description={<>This subscription delivers as its owner, <span className="font-mono">{s.created_by}</span>. Record changes reach it as that account may see them: hidden fields are removed, and records it may not see are withheld, so they never appear here.</>}>
          <Facts
            label="Subscription settings"
            items={[
              ["Topic", s.topic], ["Owner", s.created_by], ["Tenant", s.tenant ?? "Instance"], ["Delivery", s.push ? `push to ${s.push.kind} ${s.push.endpoint}` : "pull"], ...(s.push ? [["Delivery state", s.paused_at ? `paused${s.paused_reason ? `: ${s.paused_reason}` : ""}` : "delivering"] as const] : []), ["Ack deadline", duration(s.ack_deadline_ms)],
              ["Max attempts", s.max_delivery_attempts ?? "unlimited"], ["Backoff", `${duration(s.backoff_base_ms)} up to ${duration(s.backoff_max_ms)}`],
              ["Dead-letter topic", s.dead_letter_topic ?? "none"], ["Ordering", s.ordering ? "per ordering key" : "off"], ["Filter", s.filter ? JSON.stringify(s.filter) : "none"],
            ]}
          />
        </Section>
      )}
      {s?.push && <PushTools sub={s} metrics={m} onChanged={refresh} />}
      {s?.push && <AttemptLog sub={sub} onChanged={refresh} />}
      <DeadLetters sub={sub} items={dead.data} error={dead.error} onChanged={refresh} />
      <Section title="Replay" description="Seeking puts messages back so they are delivered again, as far back as the topic's retention reaches. Save a snapshot before a risky deploy to come back to it.">
        <div className="grid gap-4 text-sm md:grid-cols-3">
          <form onSubmit={(e) => { e.preventDefault(); void replay({ time: new Date(time).toISOString() }); }} className="rounded-lg border border-border p-3">
            <label htmlFor={`${uid}time`} className={lbl}>Replay from</label>
            <p className="mb-2 text-xs text-muted-foreground">Every message published at or after this time is delivered again.</p>
            <div className="flex flex-wrap gap-2">
              <Input id={`${uid}time`} size="sm" type="datetime-local" className="min-w-0 flex-1" value={time} onChange={(e) => setTime(e.target.value)} />
              <Button type="submit" disabled={!time || Number.isNaN(new Date(time).getTime())}>Replay from this time</Button>
            </div>
          </form>
          <form onSubmit={(e) => { e.preventDefault(); void replay({ snapshot: snapPick }); }} className="rounded-lg border border-border p-3">
            <span className={lbl}>Restore a snapshot</span>
            <p className="mb-2 text-xs text-muted-foreground">Puts the cursor back where the snapshot saved it.</p>
            <div className="flex flex-wrap gap-2">
              <Sel aria-label="Snapshot" className="min-w-0 flex-1" value={snapPick} onChange={setSnapPick} placeholder={mine.length ? "Choose a snapshot" : "No snapshots yet"} options={mine.map((x) => ({ value: x.name, label: `${x.name} (cursor ${x.cursor_seq}, ${x.unacked} unacked)` }))} />
              <Button type="submit" disabled={!snapPick}>Restore snapshot</Button>
              {snapPick && <ConfirmDialog trigger={<Button tone="danger" type="button">Delete snapshot</Button>} title={`Delete snapshot ${snapPick}?`} consequence="You can no longer replay from it." confirmLabel="Delete snapshot" confirmText={snapPick} onConfirm={() => run(async () => { await deleteQueueSnapshot(snapPick); setSnapPick(""); }, () => notify.deleted(`Snapshot ${snapPick}`))} />}
            </div>
          </form>
          <form onSubmit={(e) => { e.preventDefault(); void run(() => createQueueSnapshot(snapName.trim(), sub, s?.tenant ?? undefined), () => notify.created(`Snapshot ${snapName.trim()}`)).then(() => setSnapName("")); }} className="rounded-lg border border-border p-3">
            <label htmlFor={`${uid}snap`} className={lbl}>New snapshot</label>
            <p className="mb-2 text-xs text-muted-foreground">Saves where this subscription is now, to replay from later.</p>
            <div className="flex flex-wrap gap-2">
              <Input id={`${uid}snap`} size="sm" className="min-w-0 flex-1" placeholder="before-deploy" value={snapName} onChange={(e) => setSnapName(e.target.value)} />
              <Button type="submit" disabled={!snapName.trim()}>Save snapshot</Button>
            </div>
          </form>
        </div>
      </Section>
      <QueueRules kind="queue_subscription" name={sub} />
    </div>
  );
}
