import { useState } from "react";
import { Link } from "react-router-dom";
import { JOB_STATUSES, RESERVED_KIND_PREFIXES, jobStats, listJobQueues, listJobWorkers, listJobs, pauseJobQueue, resumeJobQueue } from "../../api";
import type { JobStatus } from "../../api";
import { Button } from "../../components/ui";
import { EmptyState } from "../../components/EmptyState";
import { ErrorPanel } from "../../components/ErrorPanel";
import { Pager } from "../../components/Pager";
import { SkeletonTable } from "../../components/Skeleton";
import { useLoad } from "../../hooks";
import { notify } from "../../toaster";
import StatusBadge from "./StatusBadge";
import Tenant from "./Tenant";
import { CardBody, HEAD_ROW, TableCard } from "../../components/TableCard";
import { Dot, NUM, When, type Tone } from "./ops";
import { Table } from "panelui-web/table";
import { Input } from "panelui-web/input";
import { Sel } from "../../components/controls";

const PAGE = 25;

const STATUS_TONE: Record<JobStatus, Tone> = { queued: "info", running: "warn", succeeded: "ok", dead: "bad", cancelled: "neutral" };
const STATUS_WORDS: Record<JobStatus, string> = { queued: "Queued", running: "Running", succeeded: "Succeeded", dead: "Dead", cancelled: "Cancelled" };

/** The five totals as cards; choosing one filters the job list to that status (choosing it again clears the filter). */
function Totals({ totals, active, onPick }: { totals: Record<string, number>; active: JobStatus | ""; onPick: (s: JobStatus | "") => void }) {
  return (
    <div role="group" aria-label="Job totals" className="grid grid-cols-2 gap-3 sm:grid-cols-3 lg:grid-cols-5">
      {JOB_STATUSES.map((s) => {
        const on = active === s;
        const n = totals[s] ?? 0;
        return (
          <button
            key={s}
            type="button"
            aria-pressed={on}
            title={on ? "Show every status" : `Show ${s} jobs only`}
            onClick={() => onPick(on ? "" : s)}
            className={`min-w-0 rounded-xl border bg-card px-4 py-3 text-left transition-colors hover:bg-muted/50 focus-visible:outline-2 focus-visible:outline-ring ${on ? "border-primary ring-1 ring-primary" : "border-border"}`}
          >
            <span className="flex items-center gap-1.5 text-xs font-medium text-muted-foreground"><Dot tone={STATUS_TONE[s]} pulse={s === "running" && n > 0} />{STATUS_WORDS[s]}</span>
            <span className={`mt-1 block text-lg font-semibold tabular-nums tracking-tight ${s === "dead" && n > 0 ? "text-destructive" : ""}`}>{n.toLocaleString()}</span>
          </button>
        );
      })}
    </div>
  );
}

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
      {stats.data && <Totals totals={stats.data.totals} active={status} onPick={(s) => filter(() => setStatus(s))()} />}

      <TableCard
        title="Job queues"
        count={stats.data?.queues.length}
        description="A job queue is a lane that background jobs wait in; each can limit how many run at once. They are not the topics of the Queues studio."
        actions={<Button onClick={refresh}>Refresh</Button>}
        footer={
          <p className="text-xs text-muted-foreground">
            Limits you set on a queue apply to your tenant&apos;s jobs only. You see the jobs, queues and workers you created in your tenant; the permission <code className="font-mono">engine:read_jobs:all</code> shows every one of them.
          </p>
        }
      >
        <CardBody className="space-y-3">
          <ErrorPanel error={stats.error ?? queueError} onRetry={stats.reload} />
          {stats.loading && !stats.data && !stats.error && <SkeletonTable rows={2} columns={8} label="Loading queues" />}
          {stats.data && stats.data.queues.length === 0 && (
            <EmptyState compact title="No job queues yet" why="A queue appears when the first job is enqueued, from your code (see Use from code on a job) or from a schedule." />
          )}
        </CardBody>
        {stats.data && stats.data.queues.length > 0 && (
            <Table framed density="compact" aria-label="Queues">
              <Table.Header className={HEAD_ROW}>
                <Table.Row><Table.Head className="py-1">Queue</Table.Head>{JOB_STATUSES.map((s) => <Table.Head key={s} className={NUM}>{STATUS_WORDS[s]}</Table.Head>)}<Table.Head>Tenant</Table.Head><Table.Head>Limits</Table.Head><Table.Head><span className="sr-only">Actions</span></Table.Head></Table.Row>
              </Table.Header>
              <Table.Body>
                {stats.data.queues.map((q) => {
                  const cfg = cfgOf(q.queue);
                  const limits = cfg ? [cfg.concurrency ? `${cfg.concurrency} at once` : "", cfg.rate_limit ? `${cfg.rate_limit}/${cfg.rate_period}` : ""].filter(Boolean).join(", ") : "";
                  return (
                    <Table.Row key={q.queue}>
                      <Table.Cell className="py-2">
                        <button type="button" className="font-medium text-primary hover:underline" aria-label={`Show jobs of ${q.queue}`} onClick={filter(() => setQueue(q.queue))}>{q.queue}</button>
                        {cfg?.paused && <span className="ml-2 inline-flex items-center gap-1 rounded-full border border-warning/40 bg-warning/10 px-1.5 py-0.5 text-xs font-medium text-warning">paused</span>}
                      </Table.Cell>
                      {JOB_STATUSES.map((s) => <Table.Cell key={s} className={`${NUM} ${q[s] === 0 ? "text-muted-foreground" : s === "dead" ? "font-medium text-destructive" : ""}`}>{q[s].toLocaleString()}</Table.Cell>)}
                      <Table.Cell>{cfg ? <Tenant value={cfg.tenant} /> : <span className="text-muted-foreground">none</span>}</Table.Cell>
                      <Table.Cell className="text-muted-foreground">{cfg ? limits || "no limits" : ""}</Table.Cell>
                      <Table.Cell className="text-right">
                        {cfg && <Button aria-label={`${cfg.paused ? "Resume" : "Pause"} queue ${q.queue}`} onClick={() => void toggle(q.queue, cfg.paused)}>{cfg.paused ? "Resume" : "Pause"}</Button>}
                      </Table.Cell>
                    </Table.Row>
                  );
                })}
              </Table.Body>
            </Table>
        )}
      </TableCard>

      <TableCard
        title="Jobs"
        count={jobs.data ? total.toLocaleString() : undefined}
        footer={jobs.data && total > 0 ? <Pager page={offset / PAGE + 1} perPage={PAGE} total={total} onPage={(p) => setOffset((p - 1) * PAGE)} label="Jobs pages" /> : undefined}
        actions={
          <>
            <Sel aria-label="Status" className="w-40" value={status} onChange={(v) => filter(() => setStatus(v as JobStatus | ""))()} options={[{ value: "", label: "All statuses" }, ...JOB_STATUSES.map((s) => ({ value: s, label: s === "dead" ? "Dead (dead-letter)" : STATUS_WORDS[s] }))]} />
            <Input size="sm" aria-label="Queue" placeholder="Queue" className="w-32" value={queue} onChange={(e) => filter(() => setQueue(e.target.value))()} />
            <Input size="sm" aria-label="Kind" placeholder="Kind" className="w-40" value={kind} onChange={(e) => filter(() => setKind(e.target.value))()} />
            {filtered && <Button onClick={filter(() => { setStatus(""); setQueue(""); setKind(""); })}>Clear filters</Button>}
          </>
        }
      >
        <CardBody className="space-y-3">
          <ErrorPanel error={jobs.error} onRetry={jobs.reload} />
          {jobs.loading && !jobs.data && !jobs.error && <SkeletonTable rows={5} columns={7} label="Loading jobs" />}
          {jobs.data && jobs.data.items.length === 0 && (
            <EmptyState compact
              title={filtered ? "No jobs match" : "No jobs yet"}
              why={filtered ? "Clear a filter to see more jobs." : (
                <>A job is one unit of background work with retries and progress. Enqueue one from your code, or let a schedule do it. Kinds that start with {RESERVED_KIND_PREFIXES.map((p) => `${p}`).join(" ")} are reserved for the server and refused (403) unless an administrator enqueues them.</>
              )} />
          )}
        </CardBody>
        {jobs.data && jobs.data.items.length > 0 && (
            <Table framed density="compact" aria-label="Jobs">
              <Table.Header className={HEAD_ROW}>
                <Table.Row><Table.Head className="py-2">Job</Table.Head><Table.Head>Kind</Table.Head><Table.Head>Queue</Table.Head><Table.Head>Status</Table.Head><Table.Head className={NUM}>Attempts</Table.Head><Table.Head>Tenant</Table.Head><Table.Head className="text-right">Created</Table.Head></Table.Row>
              </Table.Header>
              <Table.Body>
                {jobs.data.items.map((j) => (
                  <Table.Row key={j.id}>
                    <Table.Cell className="max-w-56 py-2"><Link className="block truncate font-mono text-xs text-primary hover:underline" title={j.id} to={`/jobs/j/${encodeURIComponent(j.id)}`}>{j.id}</Link></Table.Cell>
                    <Table.Cell className="font-medium">{j.kind}</Table.Cell>
                    <Table.Cell className="text-muted-foreground">{j.queue}</Table.Cell>
                    <Table.Cell className="whitespace-nowrap"><StatusBadge status={j.status} />{j.cancel_requested && <span className="ml-1 text-xs text-warning">cancelling</span>}</Table.Cell>
                    <Table.Cell className={`${NUM} ${j.attempts > 1 ? "text-warning" : ""}`}>{j.attempts}/{j.max_attempts}</Table.Cell>
                    <Table.Cell><Tenant value={j.tenant} /></Table.Cell>
                    <Table.Cell className="text-right text-muted-foreground"><When iso={j.created_at} /></Table.Cell>
                  </Table.Row>
                ))}
              </Table.Body>
            </Table>
        )}
      </TableCard>

      <TableCard
        title="Workers"
        count={workers.data?.length}
        description={<>A worker is a process that claims jobs and runs them. Its id belongs to the tenant and the account that registered it: registering an id someone else holds answers 409 worker_id_taken.</>}
        actions={<Button onClick={workers.reload}>Refresh</Button>}
      >
        <CardBody className="space-y-3">
          <ErrorPanel error={workers.error} onRetry={workers.reload} />
          {workers.loading && !workers.data && !workers.error && <SkeletonTable rows={2} columns={6} label="Loading workers" />}
          {workers.data && workers.data.length === 0 && <EmptyState compact title="No workers registered" why="Workers register themselves when they start. A job stays queued until one claims it." />}
        </CardBody>
        {workers.data && workers.data.length > 0 && (
            <Table framed density="compact" aria-label="Workers">
              <Table.Header className={HEAD_ROW}>
                <Table.Row><Table.Head className="py-1">Worker</Table.Head><Table.Head>State</Table.Head><Table.Head>Queues</Table.Head><Table.Head className={NUM}>At once</Table.Head><Table.Head>Tenant</Table.Head><Table.Head>Registered by</Table.Head><Table.Head className="text-right">Last seen</Table.Head></Table.Row>
              </Table.Header>
              <Table.Body>
                {workers.data.map((w) => (
                  <Table.Row key={w.id}>
                    <Table.Cell className="py-2"><span className="font-mono text-xs">{w.id}</span>{w.name ? <span className="ml-2 text-muted-foreground">{w.name}</span> : null}</Table.Cell>
                    <Table.Cell>
                      <span className={`inline-flex items-center gap-1.5 ${w.online ? "text-success" : "text-muted-foreground"}`}><Dot tone={w.online ? "ok" : "neutral"} />{w.online ? "online" : "offline"}</span>
                    </Table.Cell>
                    <Table.Cell>{w.queues.length ? w.queues.join(", ") : <span className="text-muted-foreground">all</span>}</Table.Cell>
                    <Table.Cell className={NUM}>{w.concurrency}</Table.Cell>
                    <Table.Cell><Tenant value={w.tenant} /></Table.Cell>
                    <Table.Cell className="font-mono text-xs text-muted-foreground">{w.created_by ?? "unknown"}</Table.Cell>
                    <Table.Cell className="text-right text-muted-foreground"><When iso={w.last_seen_at} /></Table.Cell>
                  </Table.Row>
                ))}
              </Table.Body>
            </Table>
        )}
      </TableCard>
    </div>
  );
}
