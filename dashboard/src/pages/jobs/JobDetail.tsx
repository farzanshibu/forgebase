import { useState } from "react";
import { Link, useNavigate, useParams } from "react-router-dom";
import { cancelJob, deleteJob, getJob, jobAttempts, jobCheckpoint, jobEvents, jobResult, retryJob } from "../../api";
import type { Job, JobResult } from "../../api";
import { Button, ErrorView } from "../../components/ui";
import { ConfirmDialog } from "../../components/ConfirmDialog";
import { EmptyState } from "../../components/EmptyState";
import { PageHeader } from "../../components/PageHeader";
import { SkeletonLines } from "../../components/Skeleton";
import { UseFromCodeButton } from "../../components/UseFromCode";
import { notify } from "../../toaster";
import Tenant from "./Tenant";
import { useLoad, usePolling } from "../../hooks";
import { when } from "../functions/util";
import ProgressBar from "./ProgressBar";
import StatusBadge from "./StatusBadge";
import { CardBody, HEAD_ROW, TableCard } from "../../components/TableCard";
import { Callout, Code, CopyIcon, Dot, Facts, NUM, Section, StatGrid, When, type Tone } from "./ops";
import { elapsed } from "./time";
import { Table } from "panelui-web/table";

const json = (v: unknown) => JSON.stringify(v, null, 2);

const EVENT_TONE: Record<string, Tone> = { succeeded: "ok", completed: "ok", failed: "bad", dead: "bad", error: "bad", cancelled: "neutral", cancel_requested: "warn", retried: "info", retry: "info", enqueued: "info", claimed: "warn", started: "warn" };
const OUTCOME_TONE: Record<string, Tone> = { succeeded: "ok", success: "ok", failed: "bad", error: "bad", lease_expired: "warn", timeout: "bad", cancelled: "neutral" };

/** How long the job ran: finished minus started, or so far while it runs. */
function runTime(j: Job): string | undefined {
  if (!j.started_at) return undefined;
  const end = j.finished_at ? Date.parse(j.finished_at) : undefined;
  const start = Date.parse(j.started_at);
  if (Number.isNaN(start)) return undefined;
  if (end === undefined) return j.status === "running" ? "running" : undefined;
  return elapsed(end - start);
}

export default function JobDetail() {
  const { id = "" } = useParams();
  const nav = useNavigate();
  const [last, setLast] = useState<Job>();
  const every = !last || last.status === "queued" || last.status === "running" ? 2000 : 30000;
  const job = usePolling((s) => getJob(id, s), every);
  const j = job.data;
  if (j && j !== last) setLast(j);
  // Live view: poll quickly while the task can still change, slowly once it has ended.
  const stamp = `${id}:${j?.status}:${j?.attempts}:${j?.checkpoint_at ?? ""}`;
  const events = useLoad((s) => jobEvents(id, s), `job-events:${stamp}`);
  const attempts = useLoad((s) => jobAttempts(id, s), `job-attempts:${stamp}`);
  const checkpoint = useLoad((s) => (j?.has_checkpoint ? jobCheckpoint(id, s) : Promise.resolve(undefined)), `job-checkpoint:${stamp}`);
  const [stored, setStored] = useState<JobResult>();
  const [error, setError] = useState<unknown>();

  async function act(fn: () => Promise<Job | void>, done: string, leave = false) {
    setError(undefined);
    try {
      await fn();
      notify.saved(`Job ${id}`, done);
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
  const ran = j ? runTime(j) : undefined;

  return (
    <div className="space-y-4">
      <PageHeader
        title={<span className="break-all font-mono text-xl">{id}</span>}
        description={j ? <span className="inline-flex flex-wrap items-center gap-x-2 gap-y-1"><CopyIcon text={id} label="Copy job id" />Job <span className="font-medium text-foreground">{j.kind}</span> in queue <span className="font-medium text-foreground">{j.queue}</span> · tenant <Tenant value={j.tenant} /></span> : "Job"}
        docs="docs/adr/0007-jobs.md"
        secondary={j ? <UseFromCodeButton kind="jobKind" name={j.kind} /> : undefined}
        primary={j ? (
          <span className="flex gap-2">
            {retryable && <Button tone="primary" onClick={() => void act(() => retryJob(j.id), "Queued again.")}>Retry</Button>}
            {cancellable && <ConfirmDialog trigger={<Button tone="danger">Cancel job</Button>} title="Cancel this job?" consequence="A queued job never runs. A running job stops at its next heartbeat." confirmLabel="Cancel job" onConfirm={() => act(() => cancelJob(j.id), "Cancel requested.")} />}
            {finished && <ConfirmDialog trigger={<Button tone="danger">Delete job</Button>} title={`Delete job ${j.id}?`} consequence="Its history, attempts and result are deleted." confirmLabel="Delete job" confirmText={j.id} onConfirm={() => act(() => deleteJob(j.id), "Deleted.", true)} />}
          </span>
        ) : undefined} />
      <ErrorView error={error ?? job.error} />
      {job.loading && !job.error && <SkeletonLines lines={5} label="Loading job" />}
      {j && (
        <>
          <StatGrid
            label="Job summary"
            items={[
              { label: "Status", value: <StatusBadge status={j.status} /> },
              { label: "Attempts", value: `${j.attempts} of ${j.max_attempts}`, tone: j.attempts > 1 && j.status !== "succeeded" ? "warn" : undefined },
              { label: "Created", value: <When iso={j.created_at} /> },
              { label: "Started", value: <When iso={j.started_at} empty="not yet" /> },
              { label: "Finished", value: <When iso={j.finished_at} empty="not yet" /> },
              { label: "Run time", value: ran ?? "—", title: j.started_at ? `Started ${when(j.started_at)}` : undefined },
            ]}
          />
          {j.workflow_id && <p className="text-sm text-muted-foreground">Step <span className="font-medium text-foreground">{j.step ?? j.kind}</span> of workflow <Link className="text-primary hover:underline" to={`/jobs/w/${encodeURIComponent(j.workflow_id)}`}>{j.workflow_id}</Link>{j.parent_id ? <> · child of <Link className="font-mono text-xs text-primary hover:underline" to={`/jobs/j/${encodeURIComponent(j.parent_id)}`}>{j.parent_id}</Link></> : null}</p>}
          {j.cancel_requested && <Callout tone="warn" role="status">Cancel requested: the worker stops at its next heartbeat.</Callout>}
          {(j.waiting_for ?? 0) > 0 && <Callout role="status">Waiting for {j.waiting_for} parent job(s) to succeed before it can start.</Callout>}
          {j.last_error && (
            <div role="alert" className="rounded-lg border border-destructive/40 bg-destructive/10 p-3 text-sm">
              <p className="break-words font-mono text-xs text-destructive">Last error: {j.last_error}</p>
            </div>
          )}
          {(j.status === "running" || j.progress) && (
            <Section title="Progress" description={j.status === "running" ? "Refreshes every 2 s while the job runs." : undefined}>
              <ProgressBar progress={j.progress} />
              {j.progress?.updated_at && <p className="mt-2 text-xs text-muted-foreground">Updated <When iso={j.progress.updated_at} /></p>}
              {j.timeout_ms ? <p className="mt-1 text-xs text-muted-foreground">Timeout {Math.round(j.timeout_ms / 1000)} s{j.deadline_at ? `, deadline ${when(j.deadline_at)}` : ""}</p> : null}
            </Section>
          )}
          <Section title="Details">
            <Facts
              label="Job details"
              items={[
                ["Queue", j.queue], ["Kind", <span key="k" className="font-mono text-xs">{j.kind}</span>], ["Priority", j.priority], ["Attempts", `${j.attempts} of ${j.max_attempts}`],
                ["Run at", <When key="r" iso={j.run_at} />], ["Created", <When key="c" iso={j.created_at} />],
                ["Started", <When key="s" iso={j.started_at} />], ["Finished", <When key="f" iso={j.finished_at} />],
                ["Worker", j.worker_id ? <span key="w" className="font-mono text-xs">{j.worker_id}</span> : "none"], ["Created by", <span key="b" className="font-mono text-xs">{j.created_by}</span>],
                ["Tenant", j.tenant ?? "instance"], ["Idempotency key", j.idempotency_key ? <span key="i" className="break-all font-mono text-xs">{j.idempotency_key}</span> : "none"],
                ["Schedule", j.schedule_id ? <span key="sc" className="font-mono text-xs">{j.schedule_id}</span> : "none"],
              ]}
            />
          </Section>
          <div className="grid gap-4 lg:grid-cols-2">
            <Section title="Payload" label="Job payload" actions={<CopyIcon text={json(j.payload)} label="Copy payload" />}><Code label="Payload">{json(j.payload)}</Code></Section>
            <Section title="Result" label="Job result" actions={stored || (j.result !== undefined && j.result !== null) ? <CopyIcon text={json(stored ? stored.result : j.result)} label="Copy result" /> : undefined}>
              <Code label="Result">{stored ? json(stored.result) : j.result === undefined || j.result === null ? (j.result_stored ? "stored outside the job record" : "none") : json(j.result)}</Code>
              {j.result_stored && !stored && (
                <div className="mt-2"><Button onClick={() => void jobResult(j.id).then(setStored).catch(setError)}>Load stored result{j.result_size ? ` (${j.result_size} bytes)` : ""}</Button></div>
              )}
            </Section>
          </div>
          {j.has_checkpoint && (
            <Section title="Checkpoint" label="Job checkpoint" description={<>Saved <When iso={checkpoint.data?.saved_at ?? j.checkpoint_at} />; the next attempt resumes from it.</>}>
              <ErrorView error={checkpoint.error} />
              <Code label="Checkpoint">{checkpoint.data ? json(checkpoint.data.checkpoint) : "loading"}</Code>
            </Section>
          )}
          <TableCard title="Attempts" count={attempts.data?.length}>
            <CardBody className="space-y-3">
              <ErrorView error={attempts.error} />
              {attempts.data && attempts.data.length === 0 && <EmptyState compact title="No attempts yet" why="An attempt starts when a worker claims the job." />}
            </CardBody>
            {attempts.data && attempts.data.length > 0 && (
                <Table framed density="compact" aria-label="Attempts">
                  <Table.Header className={HEAD_ROW}><Table.Row><Table.Head className={`py-1 ${NUM}`}>#</Table.Head><Table.Head>Outcome</Table.Head><Table.Head>Worker</Table.Head><Table.Head>Started</Table.Head><Table.Head className={NUM}>Duration</Table.Head><Table.Head>Resumed</Table.Head></Table.Row></Table.Header>
                  <Table.Body>
                    {attempts.data.map((a) => (
                      <Table.Row key={a.attempt} className="align-top">
                        <Table.Cell className={`py-2 ${NUM}`}>{a.attempt}</Table.Cell>
                        <Table.Cell>
                          <span className="inline-flex items-center gap-1.5"><Dot tone={a.outcome ? OUTCOME_TONE[a.outcome] ?? "neutral" : "warn"} pulse={!a.outcome} />{a.outcome ?? "running"}</span>
                          {a.error ? <span className="mt-0.5 block break-words font-mono text-xs text-destructive">{a.error}</span> : null}
                        </Table.Cell>
                        <Table.Cell className="font-mono text-xs">{a.worker_id ?? "none"}</Table.Cell>
                        <Table.Cell className="text-muted-foreground"><When iso={a.started_at} /></Table.Cell>
                        <Table.Cell className={NUM}>{elapsed(a.duration_ms)}</Table.Cell>
                        <Table.Cell className="text-muted-foreground">{a.from_checkpoint ? "from checkpoint" : "fresh"}</Table.Cell>
                      </Table.Row>
                    ))}
                  </Table.Body>
                </Table>
            )}
          </TableCard>
          <Section title="History" count={events.data?.length}>
            <ErrorView error={events.error} />
            {events.data && events.data.length === 0 && <EmptyState compact title="No history yet" why="Every change of the job is recorded here." />}
            <ol aria-label="Job history" className="relative space-y-3 border-l border-border pl-5 text-sm">
              {events.data?.map((e) => (
                <li key={e.id} className="relative">
                  <span className="absolute -left-[1.6rem] top-1.5"><Dot tone={EVENT_TONE[e.event] ?? "neutral"} /></span>
                  <div className="flex flex-wrap items-baseline gap-x-3 gap-y-0.5">
                    <span className="font-medium">{e.event}</span>
                    <span className="text-xs text-muted-foreground">attempt {e.attempt}</span>
                    {e.worker_id && <span className="font-mono text-xs text-muted-foreground">{e.worker_id}</span>}
                    <span className="ml-auto text-xs text-muted-foreground"><When iso={e.at} /></span>
                  </div>
                  {e.message && <p className="mt-0.5 break-words text-foreground/80">{e.message}</p>}
                </li>
              ))}
            </ol>
          </Section>
        </>
      )}
    </div>
  );
}
