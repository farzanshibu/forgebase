import { useState } from "react";
import { Link, useNavigate, useParams } from "react-router-dom";
import { cancelJob, deleteJob, getJob, jobEvents, retryJob } from "../../api";
import type { Job } from "../../api";
import { Button, ConfirmButton, ErrorView, Loading, Panel } from "../../components/ui";
import { useLoad } from "../../hooks";
import { when } from "../functions/util";
import StatusBadge from "./StatusBadge";

const json = (v: unknown) => JSON.stringify(v, null, 2);

export default function JobDetail() {
  const { id = "" } = useParams();
  const nav = useNavigate();
  const job = useLoad((s) => getJob(id, s), `job:${id}`);
  const events = useLoad((s) => jobEvents(id, s), `job-events:${id}`);
  const [error, setError] = useState<unknown>();

  async function act(fn: () => Promise<Job | void>, leave = false) {
    setError(undefined);
    try {
      await fn();
      if (leave) return nav("/jobs");
      job.reload();
      events.reload();
    } catch (e) {
      setError(e);
    }
  }

  const j = job.data;
  const retryable = j && (j.status === "dead" || j.status === "cancelled");
  const cancellable = j && (j.status === "queued" || j.status === "running") && !j.cancel_requested;
  const finished = j && (j.status === "succeeded" || j.status === "dead" || j.status === "cancelled");

  return (
    <div className="space-y-4">
      <Link to="/jobs" className="text-sm text-amber-300 hover:underline">← All jobs</Link>
      <ErrorView error={error ?? job.error} />
      {job.loading && !job.error && <Loading what="Loading job" />}
      {j && (
        <>
          <div className="flex flex-wrap items-center justify-between gap-2">
            <h2 className="flex items-center gap-2 font-mono text-lg">{j.id} <StatusBadge status={j.status} /></h2>
            <div className="flex gap-2">
              {retryable && <Button onClick={() => void act(() => retryJob(j.id))}>Retry</Button>}
              {cancellable && <Button tone="danger" onClick={() => void act(() => cancelJob(j.id))}>Cancel job</Button>}
              {finished && <ConfirmButton label="Delete job" onConfirm={() => void act(() => deleteJob(j.id), true)} />}
            </div>
          </div>
          {j.cancel_requested && <p role="status" className="text-sm text-amber-300">Cancel requested: the worker stops at its next heartbeat.</p>}
          <Panel title="Details">
            <dl aria-label="Job details" className="grid gap-x-6 gap-y-1 text-sm md:grid-cols-2">
              {([
                ["Queue", j.queue], ["Kind", j.kind], ["Priority", j.priority], ["Attempts", `${j.attempts} of ${j.max_attempts}`],
                ["Run at", when(j.run_at)], ["Created", when(j.created_at)], ["Started", when(j.started_at)], ["Finished", when(j.finished_at)],
                ["Worker", j.worker_id ?? "none"], ["Created by", j.created_by], ["Idempotency key", j.idempotency_key ?? "none"], ["Schedule", j.schedule_id ?? "none"],
              ] as [string, string | number][]).map(([k, v]) => (
                <div key={k} className="flex gap-2"><dt className="w-32 shrink-0 text-zinc-500">{k}</dt><dd>{v}</dd></div>
              ))}
            </dl>
            {j.last_error && <p className="mt-3 text-sm text-red-300">Last error: {j.last_error}</p>}
          </Panel>
          <div className="grid gap-4 md:grid-cols-2">
            <Panel title="Payload"><pre aria-label="Payload" className="overflow-x-auto font-mono text-xs">{json(j.payload)}</pre></Panel>
            <Panel title="Result"><pre aria-label="Result" className="overflow-x-auto font-mono text-xs">{j.result === undefined || j.result === null ? "none" : json(j.result)}</pre></Panel>
          </div>
          <Panel title="History">
            <ErrorView error={events.error} />
            {events.data && events.data.length === 0 && <p className="text-sm text-zinc-400">No events.</p>}
            <ol aria-label="Job history" className="space-y-1 text-sm">
              {events.data?.map((e) => (
                <li key={e.id} className="flex flex-wrap gap-x-3">
                  <span className="text-zinc-500">{when(e.at)}</span><span>{e.event}</span><span>attempt {e.attempt}</span>
                  {e.worker_id && <span className="font-mono text-xs text-zinc-500">{e.worker_id}</span>}
                  {e.message && <span className="text-zinc-300">{e.message}</span>}
                </li>
              ))}
            </ol>
          </Panel>
        </>
      )}
    </div>
  );
}
