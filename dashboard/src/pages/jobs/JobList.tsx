import { useState } from "react";
import { Link } from "react-router-dom";
import { JOB_STATUSES, RESERVED_KIND_PREFIXES, jobStats, listJobQueues, listJobWorkers, listJobs, pauseJobQueue, resumeJobQueue } from "../../api";
import type { JobStatus } from "../../api";
import { Button, Panel } from "../../components/ui";
import { EmptyState } from "../../components/EmptyState";
import { ErrorPanel } from "../../components/ErrorPanel";
import { Pager } from "../../components/Pager";
import { SkeletonTable } from "../../components/Skeleton";
import { useLoad } from "../../hooks";
import { notify } from "../../toaster";
import { when } from "../functions/util";
import StatusBadge from "./StatusBadge";
import Tenant from "./Tenant";
import { Table } from "panelui-web/table";
import { Input } from "panelui-web/input";
import { Sel } from "../../components/controls";

const PAGE = 25;

export default function JobList() {
  const [status, setStatus] = useState<JobStatus | "">("");
  const [queue, setQueue] = useState("");
  const [kind, setKind] = useState("");
  const [offset, setOffset] = useState(0);
  const stats = useLoad(jobStats, "job-stats");
  const settings = useLoad(listJobQueues, "job-queue-settings");
  const workers = useLoad(listJobWorkers, "job-workers");
  const [queueError, setQueueError] = useState<unknown>();
  async function toggle(name: string, paused: boolean) {
    setQueueError(undefined);
    try {
      await (paused ? resumeJobQueue(name) : pauseJobQueue(name));
      notify.saved(`Queue ${name}`, paused ? "It is running again." : "No new job starts until you resume it.");
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
    workers.reload();
  };
  const total = jobs.data?.total ?? 0;
  const filtered = !!(status || queue || kind);
  const cfgOf = (name: string) => settings.data?.find((x) => x.name === name);

  return (
    <div className="space-y-4">
      <Panel title="Job queues" actions={<Button onClick={refresh}>Refresh</Button>}>
        <p className="mb-3 text-xs text-muted-foreground">
          A job queue is a lane that background jobs wait in; each can limit how many run at once. They are not the topics of the Queues studio.
        </p>
        {stats.data && (
          <p aria-label="Job totals" className="mb-3 text-sm text-muted-foreground">
            {JOB_STATUSES.map((s) => `${stats.data!.totals[s] ?? 0} ${s}`).join(" · ")}
          </p>
        )}
        <ErrorPanel error={stats.error ?? queueError} onRetry={stats.reload} />
        {stats.loading && !stats.error && <SkeletonTable rows={2} columns={8} label="Loading queues" />}
        {stats.data && stats.data.queues.length === 0 && (
          <EmptyState compact title="No job queues yet" why="A queue appears when the first job is enqueued, from your code (see Use from code on a job) or from a schedule." />
        )}
        {stats.data && stats.data.queues.length > 0 && (
          <Table aria-label="Queues" className="w-full text-left text-sm">
            <Table.Header className="text-xs uppercase tracking-wide text-muted-foreground">
              <Table.Row><Table.Head className="py-1">Queue</Table.Head>{JOB_STATUSES.map((s) => <Table.Head key={s}>{s}</Table.Head>)}<Table.Head>Tenant</Table.Head><Table.Head>Limits</Table.Head><Table.Head /></Table.Row>
            </Table.Header>
            <Table.Body>
              {stats.data.queues.map((q) => {
                const cfg = cfgOf(q.queue);
                const limits = cfg ? [cfg.concurrency ? `${cfg.concurrency} at once` : "", cfg.rate_limit ? `${cfg.rate_limit}/${cfg.rate_period}` : ""].filter(Boolean).join(", ") : "";
                return (
                  <Table.Row key={q.queue} className="border-t border-border">
                    <Table.Cell className="py-1.5">
                      <button type="button" className="text-primary hover:underline" aria-label={`Show jobs of ${q.queue}`} onClick={filter(() => setQueue(q.queue))}>{q.queue}</button>
                    </Table.Cell>
                    {JOB_STATUSES.map((s) => <Table.Cell key={s}>{q[s]}</Table.Cell>)}
                    <Table.Cell>{cfg ? <Tenant value={cfg.tenant} /> : <span className="text-muted-foreground">none</span>}</Table.Cell>
                    <Table.Cell className="text-muted-foreground">{cfg ? <>{limits || "no limits"}{cfg.paused ? <span className="ml-1 text-warning">paused</span> : null}</> : ""}</Table.Cell>
                    <Table.Cell>
                      {cfg && <button type="button" className="text-primary hover:underline" aria-label={`${cfg.paused ? "Resume" : "Pause"} queue ${q.queue}`} onClick={() => void toggle(q.queue, cfg.paused)}>{cfg.paused ? "Resume" : "Pause"}</button>}
                    </Table.Cell>
                  </Table.Row>
                );
              })}
            </Table.Body>
          </Table>
        )}
        <p className="mt-3 text-xs text-muted-foreground">
          Limits you set on a queue apply to your tenant&apos;s jobs only. You see the jobs, queues and workers you created in your tenant; the permission <code className="font-mono">engine:read_jobs:all</code> shows every one of them.
        </p>
      </Panel>

      <div className="flex flex-wrap items-end gap-3 text-sm">
        <label>Status
          <Sel aria-label="Status" className="ml-2" value={status} onChange={(v) => filter(() => setStatus(v as JobStatus | ""))()} options={[{ value: "", label: "all" }, ...JOB_STATUSES.map((s) => ({ value: s, label: s === "dead" ? "dead (dead-letter)" : s }))]} />
        </label>
        <label>Queue<Input size="sm" className="ml-2 w-36" value={queue} onChange={(e) => filter(() => setQueue(e.target.value))()} /></label>
        <label>Kind<Input size="sm" className="ml-2 w-36" value={kind} onChange={(e) => filter(() => setKind(e.target.value))()} /></label>
      </div>

      <ErrorPanel error={jobs.error} onRetry={jobs.reload} />
      {jobs.loading && !jobs.error && <SkeletonTable rows={5} columns={7} label="Loading jobs" />}
      {jobs.data && jobs.data.items.length === 0 && (
        <EmptyState compact
          title={filtered ? "No jobs match" : "No jobs yet"}
          why={filtered ? "Clear a filter to see more jobs." : (
            <>A job is one unit of background work with retries and progress. Enqueue one from your code, or let a schedule do it. Kinds that start with {RESERVED_KIND_PREFIXES.map((p) => `${p}`).join(" ")} are reserved for the server and refused (403) unless an administrator enqueues them.</>
          )} />
      )}
      {jobs.data && jobs.data.items.length > 0 && (
        <Table aria-label="Jobs" className="w-full text-left text-sm">
          <Table.Header className="text-xs uppercase tracking-wide text-muted-foreground">
            <Table.Row><Table.Head className="py-2">Job</Table.Head><Table.Head>Queue</Table.Head><Table.Head>Kind</Table.Head><Table.Head>Status</Table.Head><Table.Head>Attempts</Table.Head><Table.Head>Tenant</Table.Head><Table.Head>Created</Table.Head></Table.Row>
          </Table.Header>
          <Table.Body>
            {jobs.data.items.map((j) => (
              <Table.Row key={j.id} className="border-t border-border">
                <Table.Cell className="py-2"><Link className="font-mono text-xs text-primary hover:underline" to={`/jobs/j/${encodeURIComponent(j.id)}`}>{j.id}</Link></Table.Cell>
                <Table.Cell>{j.queue}</Table.Cell><Table.Cell>{j.kind}</Table.Cell>
                <Table.Cell><StatusBadge status={j.status} />{j.cancel_requested && <span className="ml-1 text-xs text-warning">cancelling</span>}</Table.Cell>
                <Table.Cell>{j.attempts}/{j.max_attempts}</Table.Cell>
                <Table.Cell><Tenant value={j.tenant} /></Table.Cell>
                <Table.Cell className="text-muted-foreground">{when(j.created_at)}</Table.Cell>
              </Table.Row>
            ))}
          </Table.Body>
        </Table>
      )}
      {jobs.data && total > 0 && <Pager page={offset / PAGE + 1} perPage={PAGE} total={total} onPage={(p) => setOffset((p - 1) * PAGE)} label="Jobs pages" />}

      <Panel title="Workers" actions={<Button onClick={workers.reload}>Refresh</Button>}>
        <p className="mb-3 text-xs text-muted-foreground">A worker is a process that claims jobs and runs them. Its id belongs to the tenant and the account that registered it: registering an id someone else holds answers 409 worker_id_taken.</p>
        <ErrorPanel error={workers.error} onRetry={workers.reload} />
        {workers.loading && !workers.error && <SkeletonTable rows={2} columns={6} label="Loading workers" />}
        {workers.data && workers.data.length === 0 && <EmptyState compact title="No workers registered" why="Workers register themselves when they start. A job stays queued until one claims it." />}
        {workers.data && workers.data.length > 0 && (
          <Table aria-label="Workers" className="w-full text-left text-sm">
            <Table.Header className="text-xs uppercase tracking-wide text-muted-foreground">
              <Table.Row><Table.Head className="py-1">Worker</Table.Head><Table.Head>State</Table.Head><Table.Head>Queues</Table.Head><Table.Head>At once</Table.Head><Table.Head>Tenant</Table.Head><Table.Head>Registered by</Table.Head><Table.Head>Last seen</Table.Head></Table.Row>
            </Table.Header>
            <Table.Body>
              {workers.data.map((w) => (
                <Table.Row key={w.id} className="border-t border-border">
                  <Table.Cell className="py-1.5"><span className="font-mono text-xs">{w.id}</span>{w.name ? <span className="ml-2 text-muted-foreground">{w.name}</span> : null}</Table.Cell>
                  <Table.Cell>{w.online ? <span className="text-success">online</span> : <span className="text-muted-foreground">offline</span>}</Table.Cell>
                  <Table.Cell>{w.queues.length ? w.queues.join(", ") : "all"}</Table.Cell>
                  <Table.Cell>{w.concurrency}</Table.Cell>
                  <Table.Cell><Tenant value={w.tenant} /></Table.Cell>
                  <Table.Cell className="text-muted-foreground">{w.created_by ?? "unknown"}</Table.Cell>
                  <Table.Cell className="text-muted-foreground">{when(w.last_seen_at)}</Table.Cell>
                </Table.Row>
              ))}
            </Table.Body>
          </Table>
        )}
      </Panel>
    </div>
  );
}
