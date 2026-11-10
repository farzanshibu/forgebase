import { useState } from "react";
import { Link, useNavigate, useParams } from "react-router-dom";
import { createQueueSnapshot, queueDeadLetters, deleteQueueSnapshot, deleteQueueSubscription, getQueueSubscription, listQueueSnapshots, seekQueueSubscription, queueSubscriptionMetrics } from "../../api";
import { Button, ConfirmButton, ErrorView, Loading, Panel } from "../../components/ui";
import { useLoad } from "../../hooks";
import { when } from "../functions/util";
import { duration, pretty } from "./util";
import { Input } from "panelui-web/input";
import { PanelSelect } from "../panel/PanelSelect";

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
  async function run(fn: () => Promise<unknown>, done: string) {
    setError(undefined);
    setNotice("");
    try {
      await fn();
      setNotice(done);
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
      <Link to="/queues/subscriptions" className="text-sm text-primary hover:underline">← All subscriptions</Link>
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h2 className="font-mono text-lg">{sub}</h2>
        <div className="flex gap-2">
          <Button onClick={refresh}>Refresh</Button>
          <ConfirmButton label="Delete subscription" onConfirm={() => void run(async () => { await deleteQueueSubscription(sub); nav("/queues/subscriptions"); }, "")} />
        </div>
      </div>
      <ErrorView error={error ?? info.error} />
      {notice && <p role="status" className="text-sm text-success">{notice}</p>}
      {info.loading && !info.error && <Loading what="Loading subscription" />}
      {s && (
        <Panel title="Settings">
          <dl aria-label="Subscription settings" className="grid gap-x-6 gap-y-1 text-sm md:grid-cols-2">
            {([
              ["Topic", s.topic], ["Delivery", s.push ? `push to ${s.push.kind} ${s.push.endpoint}` : "pull"], ["Ack deadline", duration(s.ack_deadline_ms)],
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
        <ErrorView error={dead.error} />
        {dead.data && dead.data.length === 0 && <p className="text-sm text-muted-foreground">No dead letters.</p>}
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
            <PanelSelect aria-label="Snapshot" className="ml-2" value={snapPick} onChange={setSnapPick} options={[{ value: "", label: "choose…" }, ...mine.map((x) => ({ value: x.name, label: `${x.name} (cursor ${x.cursor_seq}, ${x.unacked} unacked)` }))]} />
          </label>
          <Button type="submit" disabled={!snapPick}>Restore snapshot</Button>
          {snapPick && <ConfirmButton label="Delete snapshot" onConfirm={() => void run(async () => { await deleteQueueSnapshot(snapPick); setSnapPick(""); }, "Snapshot deleted.")} />}
        </form>
        <form onSubmit={(e) => { e.preventDefault(); void run(() => createQueueSnapshot(snapName.trim(), sub), `Snapshot ${snapName.trim()} saved.`).then(() => setSnapName("")); }} className="mt-3 flex flex-wrap items-end gap-3 text-sm">
          <label>New snapshot<Input size="sm" className="ml-2 w-40" value={snapName} onChange={(e) => setSnapName(e.target.value)} /></label>
          <Button type="submit" disabled={!snapName.trim()}>Save snapshot</Button>
        </form>
      </Panel>
    </div>
  );
}
