import { useEffect, useState } from "react";
import { Link, useNavigate, useParams } from "react-router-dom";
import { cancelJob, deleteJob, getJob, jobAttempts, jobCheckpoint, jobEvents, jobResult, retryJob } from "../../api";
import type { Job, JobResult } from "../../api";
import { Button, ConfirmButton, ErrorView, Loading, Panel } from "../../components/ui";
import { useLoad, usePolling } from "../../hooks";
import { when } from "../functions/util";
import ProgressBar from "./ProgressBar";
import StatusBadge from "./StatusBadge";

const json = (v: unknown) => JSON.stringify(v, null, 2);

export default function JobDetail() {
  const { id = "" } = useParams();
  const nav = useNavigate();
  const [every, setEvery] = useState(2000);
  const job = usePolling((s) => getJob(id, s), every);
  const j = job.data;
  // Live view: poll quickly while the task can still change, slowly once it has ended.
  const active = !j || j.status === "queued" || j.status === "running";
  useEffect(() => setEvery(active ? 2000 : 30000), [active]);
  const stamp = `${id}:${j?.status}:${j?.attempts}:${j?.checkpoint_at ?? ""}`;
  const events = useLoad((s) => jobEvents(id, s), `job-events:${stamp}`);
  const attempts = useLoad((s) => jobAttempts(id, s), `job-attempts:${stamp}`);
  const checkpoint = useLoad((s) => (j?.has_checkpoint ? jobCheckpoint(id, s) : Promise.resolve(undefined)), `job-checkpoint:${stamp}`);
  const [stored, setStored] = useState<JobResult>();
  const [error, setError] = useState<unknown>();

  async function act(fn: () => Promise<Job | void>, leave = false) {
    setError(undefined);
    try {
      await fn();
      if (leave) return nav("/jobs");
      job.refresh();
      events.reload();
      attempts.reload();
    } catch (e) {
      setError(e);
    }
  }

  const retryable = j && (j.status === "dead" || j.status === "cancelled");
  const cancellable = j && (j.status === "queued" || j.status === "running") && !j.cancel_requested;
  const finished = j && (j.status === "succeeded" || j.status === "dead" || j.status === "cancelled");

  return (
    <div className="space-y-4">
      <Link to="/jobs" className="text-sm text-amber-300 hover:underline">← All jobs</Link>
      <ErrorView error={error ?? job.error} />
      {job.loading && !job.error && <Loading what="Loading job" />}
      {j?.workflow_id && <p className="text-sm text-zinc-400">Step {j.step ?? j.kind} of workflow <Link className="text-amber-300 hover:underline" to={`/jobs/w/${encodeURIComponent(j.workflow_id)}`}>{j.workflow_id}</Link>{j.parent_id ? <> · child of <Link className="font-mono text-xs text-amber-300 hover:underline" to={`/jobs/j/${encodeURIComponent(j.parent_id)}`}>{j.parent_id}</Link></> : null}</p>}
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
          {(j.waiting_for ?? 0) > 0 && <p role="status" className="text-sm text-zinc-300">Waiting for {j.waiting_for} parent job(s) to succeed before it can start.</p>}
          {(j.status === "running" || j.progress) && (
            <Panel title="Progress">
              <ProgressBar progress={j.progress} />
              {j.progress?.updated_at && <p className="mt-1 text-xs text-zinc-500">Updated {when(j.progress.updated_at)}{j.status === "running" ? " · refreshes every 2 s" : ""}</p>}
              {j.timeout_ms ? <p className="mt-1 text-xs text-zinc-500">Timeout {Math.round(j.timeout_ms / 1000)} s{j.deadline_at ? `, deadline ${when(j.deadline_at)}` : ""}</p> : null}
            </Panel>
          )}
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
            <Panel title="Result">
              <pre aria-label="Result" className="overflow-x-auto font-mono text-xs">{stored ? json(stored.result) : j.result === undefined || j.result === null ? (j.result_stored ? "stored outside the job record" : "none") : json(j.result)}</pre>
              {j.result_stored && !stored && (
                <Button onClick={() => void jobResult(j.id).then(setStored).catch(setError)}>Load stored result{j.result_size ? ` (${j.result_size} bytes)` : ""}</Button>
              )}
            </Panel>
          </div>
          {j.has_checkpoint && (
            <Panel title="Checkpoint">
              <ErrorView error={checkpoint.error} />
              <p className="mb-1 text-xs text-zinc-500">Saved {when(checkpoint.data?.saved_at ?? j.checkpoint_at)}; the next attempt resumes from it.</p>
              <pre aria-label="Checkpoint" className="overflow-x-auto font-mono text-xs">{checkpoint.data ? json(checkpoint.data.checkpoint) : "loading"}</pre>
            </Panel>
          )}
          <Panel title="Attempts">
            <ErrorView error={attempts.error} />
            {attempts.data && attempts.data.length === 0 && <p className="text-sm text-zinc-400">No attempts yet.</p>}
            {attempts.data && attempts.data.length > 0 && (
              <table aria-label="Attempts" className="w-full text-left text-sm">
                <thead className="text-xs uppercase tracking-wide text-zinc-500"><tr><th className="py-1">#</th><th>Worker</th><th>Started</th><th>Duration</th><th>Outcome</th><th>Resumed</th></tr></thead>
                <tbody>
                  {attempts.data.map((a) => (
                    <tr key={a.attempt} className="border-t border-zinc-800 align-top">
                      <td className="py-1">{a.attempt}</td><td className="font-mono text-xs">{a.worker_id ?? "none"}</td><td className="text-zinc-400">{when(a.started_at)}</td>
                      <td>{a.duration_ms} ms</td><td>{a.outcome ?? "running"}{a.error ? <span className="block text-xs text-red-300">{a.error}</span> : null}</td><td>{a.from_checkpoint ? "from checkpoint" : "fresh"}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            )}
          </Panel>
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
