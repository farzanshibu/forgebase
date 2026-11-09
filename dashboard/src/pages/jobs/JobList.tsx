import { useState } from "react";
import { Link } from "react-router-dom";
import { JOB_STATUSES, jobStats, listJobQueues, listJobs, pauseJobQueue, resumeJobQueue } from "../../api";
import type { JobStatus } from "../../api";
import { Button, ErrorView, Loading, Panel, inputCls } from "../../components/ui";
import { useLoad } from "../../hooks";
import { when } from "../functions/util";
import StatusBadge from "./StatusBadge";

const PAGE = 25;

export default function JobList() {
  const [status, setStatus] = useState<JobStatus | "">("");
  const [queue, setQueue] = useState("");
  const [kind, setKind] = useState("");
  const [offset, setOffset] = useState(0);
  const stats = useLoad(jobStats, "job-stats");
  const settings = useLoad(listJobQueues, "job-queue-settings");
  const [queueError, setQueueError] = useState<unknown>();
  async function toggle(name: string, paused: boolean) {
    setQueueError(undefined);
    try {
      await (paused ? resumeJobQueue(name) : pauseJobQueue(name));
      settings.reload();
    } catch (e) {
      setQueueError(e);
    }
  }
  const key = `jobs:${status}:${queue}:${kind}:${offset}`;
  const jobs = useLoad((s) => listJobs({ status: status || undefined, queue, kind, limit: PAGE, offset }, s), key);

  const filter = (f: () => void) => () => {
    f();
    setOffset(0);
  };
  const refresh = () => {
    stats.reload();
    settings.reload();
    jobs.reload();
  };
  const total = jobs.data?.total ?? 0;

  return (
    <div className="space-y-4">
      <Panel title="Queues" actions={<Button onClick={refresh}>Refresh</Button>}>
        {stats.data && (
          <p aria-label="Job totals" className="mb-3 text-sm text-zinc-400">
            {JOB_STATUSES.map((s) => `${stats.data!.totals[s] ?? 0} ${s}`).join(" · ")}
          </p>
        )}
        <ErrorView error={stats.error ?? queueError} />
        {stats.data && stats.data.queues.length === 0 && <p className="text-sm text-zinc-400">No queues yet: they appear when the first job is enqueued.</p>}
        {stats.data && stats.data.queues.length > 0 && (
          <table aria-label="Queues" className="w-full text-left text-sm">
            <thead className="text-xs uppercase tracking-wide text-zinc-500">
              <tr><th className="py-1">Queue</th>{JOB_STATUSES.map((s) => <th key={s}>{s}</th>)}<th>Limits</th><th /></tr>
            </thead>
            <tbody>
              {stats.data.queues.map((q) => (
                <tr key={q.queue} className="border-t border-zinc-800">
                  <td className="py-1.5">
                    <button type="button" className="text-amber-300 hover:underline" aria-label={`Show jobs of ${q.queue}`} onClick={filter(() => setQueue(q.queue))}>{q.queue}</button>
                  </td>
                  {JOB_STATUSES.map((s) => <td key={s}>{q[s]}</td>)}
                  {(() => {
                    const cfg = settings.data?.find((x) => x.name === q.queue);
                    if (!cfg) return <><td /><td /></>;
                    const limits = [cfg.concurrency ? `${cfg.concurrency} at once` : "", cfg.rate_limit ? `${cfg.rate_limit}/${cfg.rate_period}` : ""].filter(Boolean).join(", ");
                    return (
                      <>
                        <td className="text-zinc-400">{limits || "no limits"}{cfg.paused ? <span className="ml-1 text-amber-300">paused</span> : null}</td>
                        <td><button type="button" className="text-amber-300 hover:underline" aria-label={`${cfg.paused ? "Resume" : "Pause"} queue ${q.queue}`} onClick={() => void toggle(q.queue, cfg.paused)}>{cfg.paused ? "Resume" : "Pause"}</button></td>
                      </>
                    );
                  })()}
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </Panel>

      <div className="flex flex-wrap items-end gap-3 text-sm">
        <label>Status
          <select className={`${inputCls} ml-2`} value={status} onChange={(e) => filter(() => setStatus(e.target.value as JobStatus | ""))()}>
            <option value="">all</option>
            {JOB_STATUSES.map((s) => <option key={s} value={s}>{s === "dead" ? "dead (dead-letter)" : s}</option>)}
          </select>
        </label>
        <label>Queue<input className={`${inputCls} ml-2 w-36`} value={queue} onChange={(e) => filter(() => setQueue(e.target.value))()} /></label>
        <label>Kind<input className={`${inputCls} ml-2 w-36`} value={kind} onChange={(e) => filter(() => setKind(e.target.value))()} /></label>
      </div>

      <ErrorView error={jobs.error} />
      {jobs.loading && !jobs.error && <Loading what="Loading jobs" />}
      {jobs.data && jobs.data.items.length === 0 && <p className="text-sm text-zinc-400">No jobs match.</p>}
      {jobs.data && jobs.data.items.length > 0 && (
        <table aria-label="Jobs" className="w-full text-left text-sm">
          <thead className="text-xs uppercase tracking-wide text-zinc-500">
            <tr><th className="py-2">Job</th><th>Queue</th><th>Kind</th><th>Status</th><th>Attempts</th><th>Created</th></tr>
          </thead>
          <tbody>
            {jobs.data.items.map((j) => (
              <tr key={j.id} className="border-t border-zinc-800">
                <td className="py-2"><Link className="font-mono text-xs text-amber-300 hover:underline" to={`/jobs/j/${encodeURIComponent(j.id)}`}>{j.id}</Link></td>
                <td>{j.queue}</td><td>{j.kind}</td>
                <td><StatusBadge status={j.status} />{j.cancel_requested && <span className="ml-1 text-xs text-amber-300">cancelling</span>}</td>
                <td>{j.attempts}/{j.max_attempts}</td>
                <td className="text-zinc-400">{when(j.created_at)}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
      {jobs.data && total > 0 && (
        <div className="flex items-center gap-3 text-sm text-zinc-400">
          <Button disabled={offset === 0} onClick={() => setOffset(Math.max(0, offset - PAGE))}>Previous</Button>
          <span>{offset + 1}–{Math.min(offset + PAGE, total)} of {total}</span>
          <Button disabled={offset + PAGE >= total} onClick={() => setOffset(offset + PAGE)}>Next</Button>
        </div>
      )}
    </div>
  );
}
