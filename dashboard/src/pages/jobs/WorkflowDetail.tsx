import { useState } from "react";
import { Link, useParams } from "react-router-dom";
import { ConfirmDialog } from "../../components/ConfirmDialog";
import { PageHeader } from "../../components/PageHeader";
import { SkeletonLines } from "../../components/Skeleton";
import { notify } from "../../toaster";
import Tenant from "./Tenant";
import { cancelWorkflow, getWorkflow } from "../../api";
import { Button, ErrorView } from "../../components/ui";
import { usePolling } from "../../hooks";
import ProgressBar from "./ProgressBar";
import StatusBadge, { WorkflowStatusBadge } from "./StatusBadge";
import WorkflowGraph from "./WorkflowGraph";
import { HEAD_ROW, TableCard } from "../../components/TableCard";
import { CopyIcon, NUM, Section, StatGrid, When } from "./ops";
import { Table } from "panelui-web/table";

export default function WorkflowDetail() {
  const { id = "" } = useParams();
  const [lastStatus, setLastStatus] = useState<string>();
  const every = !lastStatus || lastStatus === "running" || lastStatus === "compensating" ? 2000 : 30000;
  const wf = usePolling((s) => getWorkflow(id, s), every);
  const [error, setError] = useState<unknown>();
  const w = wf.data;
  if (w && w.status !== lastStatus) setLastStatus(w.status);
  // Poll quickly while the workflow runs, slowly once it has ended.

  async function cancel() {
    setError(undefined);
    await cancelWorkflow(id);
    notify.saved("Workflow", "Cancelled: its queued steps will not run.");
    wf.refresh();
  }

  const done = w ? w.steps.filter((s) => s.job.status === "succeeded").length : 0;
  const failed = w ? w.steps.filter((s) => s.job.status === "dead").length : 0;

  return (
    <div className="space-y-4">
      <PageHeader
        title={w?.name ?? "Workflow"}
        description={w ? <span className="inline-flex flex-wrap items-center gap-x-2 gap-y-1">Workflow <span className="font-mono text-xs">{w.id}</span><CopyIcon text={w.id} label="Copy workflow id" /> · tenant <Tenant value={w.tenant} /></span> : "A workflow joins jobs into steps."}
        docs="docs/adr/0007-jobs.md"
        primary={w && (w.status === "running" || w.status === "compensating") ? (
          <ConfirmDialog trigger={<Button tone="danger">Cancel workflow</Button>} title="Cancel this workflow?" consequence="Steps that have not started will not run. Steps already running stop at their next heartbeat." confirmLabel="Cancel workflow" onConfirm={cancel} />
        ) : undefined} />
      <ErrorView error={error ?? wf.error} />
      {wf.loading && !wf.error && <SkeletonLines lines={4} label="Loading workflow" />}
      {w && (
        <>
          <StatGrid
            label="Workflow summary"
            items={[
              { label: "Status", value: <span role="status" aria-label="Workflow status"><WorkflowStatusBadge status={w.status} /></span> },
              { label: "Steps done", value: `${done} of ${w.steps.length}`, tone: failed > 0 ? "bad" : undefined, hint: failed > 0 ? `${failed} failed` : undefined },
              { label: "On failure", value: w.on_failure === "compensate" ? "Undo finished steps" : "Stop" },
              { label: "Created", value: <When iso={w.created_at} /> },
              { label: "Finished", value: <When iso={w.finished_at} empty="not yet" /> },
            ]}
          />
          {w.error && (
            <div role="alert" className="rounded-lg border border-destructive/40 bg-destructive/10 p-3 text-sm">
              <p className="text-destructive">Error: {w.error}{w.failed_job_id ? <> (job <Link className="font-mono text-xs underline" to={`/jobs/j/${encodeURIComponent(w.failed_job_id)}`}>{w.failed_job_id}</Link>)</> : null}</p>
            </div>
          )}
          <Section title="Graph" description="Each step starts when the steps it depends on succeed. Choose a step to open its job.">
            <WorkflowGraph steps={w.steps} />
          </Section>
          <TableCard title="Steps" count={w.steps.length}>
              <Table framed density="compact" aria-label="Workflow steps">
                <Table.Header className={HEAD_ROW}><Table.Row><Table.Head className="py-1">Step</Table.Head><Table.Head>Job</Table.Head><Table.Head>Status</Table.Head><Table.Head>Progress</Table.Head><Table.Head className={NUM}>Attempts</Table.Head></Table.Row></Table.Header>
                <Table.Body>
                  {w.steps.map((s) => (
                    <Table.Row key={s.job.id} className="align-top">
                      <Table.Cell className="py-2 font-medium">{s.name ?? s.job.kind}{s.role === "compensation" && <span className="ml-2 rounded-full border border-warning/40 bg-warning/10 px-1.5 py-0.5 text-xs font-normal text-warning">compensation</span>}</Table.Cell>
                      <Table.Cell className="max-w-48"><Link className="block truncate font-mono text-xs text-primary hover:underline" title={s.job.id} to={`/jobs/j/${encodeURIComponent(s.job.id)}`}>{s.job.id}</Link></Table.Cell>
                      <Table.Cell className="whitespace-nowrap"><StatusBadge status={s.job.status} />{(s.job.waiting_for ?? 0) > 0 && <span className="ml-2 text-xs text-muted-foreground">waiting for {s.job.waiting_for}</span>}</Table.Cell>
                      <Table.Cell className="w-64"><ProgressBar progress={s.job.progress} label={`Progress of ${s.name ?? s.job.id}`} /></Table.Cell>
                      <Table.Cell className={NUM}>{s.job.attempts}/{s.job.max_attempts}</Table.Cell>
                    </Table.Row>
                  ))}
                </Table.Body>
              </Table>
          </TableCard>
        </>
      )}
    </div>
  );
}
