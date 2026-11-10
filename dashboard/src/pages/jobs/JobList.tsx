import { useState } from "react";
import { Link } from "react-router-dom";
import { JOB_STATUSES, jobStats, listJobQueues, listJobs, pauseJobQueue, resumeJobQueue } from "../../api";
import type { JobStatus } from "../../api";
import { Button, ErrorView, Loading, Panel } from "../../components/ui";
import { useLoad } from "../../hooks";
import { when } from "../functions/util";
import StatusBadge from "./StatusBadge";
import { Table } from "panelui-web/table";
import { Input } from "panelui-web/input";
import { PanelSelect } from "../panel/PanelSelect";

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
          <p aria-label="Job totals" className="mb-3 text-sm text-muted-foreground">
            {JOB_STATUSES.map((s) => `${stats.data!.totals[s] ?? 0} ${s}`).join(" · ")}
          </p>
        )}
        <ErrorView error={stats.error ?? queueError} />
        {stats.data && stats.data.queues.length === 0 && <p className="text-sm text-muted-foreground">No queues yet: they appear when the first job is enqueued.</p>}
        {stats.data && stats.data.queues.length > 0 && (
          <Table aria-label="Queues" className="w-full text-left text-sm">
            <Table.Header className="text-xs uppercase tracking-wide text-muted-foreground">
              <Table.Row><Table.Head className="py-1">Queue</Table.Head>{JOB_STATUSES.map((s) => <Table.Head key={s}>{s}</Table.Head>)}<Table.Head>Limits</Table.Head><Table.Head /></Table.Row>
            </Table.Header>
            <Table.Body>
              {stats.data.queues.map((q) => (
                <Table.Row key={q.queue} className="border-t border-border">
                  <Table.Cell className="py-1.5">
                    <button type="button" className="text-primary hover:underline" aria-label={`Show jobs of ${q.queue}`} onClick={filter(() => setQueue(q.queue))}>{q.queue}</button>
                  </Table.Cell>
                  {JOB_STATUSES.map((s) => <Table.Cell key={s}>{q[s]}</Table.Cell>)}
                  {(() => {
                    const cfg = settings.data?.find((x) => x.name === q.queue);
                    if (!cfg) return <><Table.Cell /><Table.Cell /></>;
                    const limits = [cfg.concurrency ? `${cfg.concurrency} at once` : "", cfg.rate_limit ? `${cfg.rate_limit}/${cfg.rate_period}` : ""].filter(Boolean).join(", ");
                    return (
                      <>
                        <Table.Cell className="text-muted-foreground">{limits || "no limits"}{cfg.paused ? <span className="ml-1 text-warning">paused</span> : null}</Table.Cell>
                        <Table.Cell><button type="button" className="text-primary hover:underline" aria-label={`${cfg.paused ? "Resume" : "Pause"} queue ${q.queue}`} onClick={() => void toggle(q.queue, cfg.paused)}>{cfg.paused ? "Resume" : "Pause"}</button></Table.Cell>
                      </>
                    );
                  })()}
                </Table.Row>
              ))}
            </Table.Body>
          </Table>
        )}
      </Panel>

      <div className="flex flex-wrap items-end gap-3 text-sm">
        <label>Status
          <PanelSelect aria-label="Status" className="ml-2" value={status} onChange={(v) => filter(() => setStatus(v as JobStatus | ""))()} options={[{ value: "", label: "all" }, ...JOB_STATUSES.map((s) => ({ value: s, label: s === "dead" ? "dead (dead-letter)" : s }))]} />
        </label>
        <label>Queue<Input size="sm" className="ml-2 w-36" value={queue} onChange={(e) => filter(() => setQueue(e.target.value))()} /></label>
        <label>Kind<Input size="sm" className="ml-2 w-36" value={kind} onChange={(e) => filter(() => setKind(e.target.value))()} /></label>
      </div>

      <ErrorView error={jobs.error} />
      {jobs.loading && !jobs.error && <Loading what="Loading jobs" />}
      {jobs.data && jobs.data.items.length === 0 && <p className="text-sm text-muted-foreground">No jobs match.</p>}
      {jobs.data && jobs.data.items.length > 0 && (
        <Table aria-label="Jobs" className="w-full text-left text-sm">
          <Table.Header className="text-xs uppercase tracking-wide text-muted-foreground">
            <Table.Row><Table.Head className="py-2">Job</Table.Head><Table.Head>Queue</Table.Head><Table.Head>Kind</Table.Head><Table.Head>Status</Table.Head><Table.Head>Attempts</Table.Head><Table.Head>Created</Table.Head></Table.Row>
          </Table.Header>
          <Table.Body>
            {jobs.data.items.map((j) => (
              <Table.Row key={j.id} className="border-t border-border">
                <Table.Cell className="py-2"><Link className="font-mono text-xs text-primary hover:underline" to={`/jobs/j/${encodeURIComponent(j.id)}`}>{j.id}</Link></Table.Cell>
                <Table.Cell>{j.queue}</Table.Cell><Table.Cell>{j.kind}</Table.Cell>
                <Table.Cell><StatusBadge status={j.status} />{j.cancel_requested && <span className="ml-1 text-xs text-warning">cancelling</span>}</Table.Cell>
                <Table.Cell>{j.attempts}/{j.max_attempts}</Table.Cell>
                <Table.Cell className="text-muted-foreground">{when(j.created_at)}</Table.Cell>
              </Table.Row>
            ))}
          </Table.Body>
        </Table>
      )}
      {jobs.data && total > 0 && (
        <div className="flex items-center gap-3 text-sm text-muted-foreground">
          <Button disabled={offset === 0} onClick={() => setOffset(Math.max(0, offset - PAGE))}>Previous</Button>
          <span>{offset + 1}–{Math.min(offset + PAGE, total)} of {total}</span>
          <Button disabled={offset + PAGE >= total} onClick={() => setOffset(offset + PAGE)}>Next</Button>
        </div>
      )}
    </div>
  );
}
