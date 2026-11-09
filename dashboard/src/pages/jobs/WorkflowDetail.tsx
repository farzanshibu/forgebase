import { useState } from "react";
import { Link, useParams } from "react-router-dom";
import { cancelWorkflow, getWorkflow } from "../../api";
import { Button, ErrorView, Loading, Panel } from "../../components/ui";
import { usePolling } from "../../hooks";
import { when } from "../functions/util";
import ProgressBar from "./ProgressBar";
import StatusBadge from "./StatusBadge";
import WorkflowGraph from "./WorkflowGraph";

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
    try {
      await cancelWorkflow(id);
      wf.refresh();
    } catch (e) {
      setError(e);
    }
  }

  return (
    <div className="space-y-4">
      <Link to="/jobs/workflows" className="text-sm text-amber-300 hover:underline">← All workflows</Link>
      <ErrorView error={error ?? wf.error} />
      {wf.loading && !wf.error && <Loading what="Loading workflow" />}
      {w && (
        <>
          <div className="flex flex-wrap items-center justify-between gap-2">
            <h2 className="text-lg">{w.name} <span role="status" aria-label="Workflow status" className="ml-2 rounded bg-zinc-800 px-2 py-0.5 text-xs">{w.status}</span></h2>
            {(w.status === "running" || w.status === "compensating") && <Button tone="danger" onClick={() => void cancel()}>Cancel workflow</Button>}
          </div>
          <p className="text-sm text-zinc-400">On failure: {w.on_failure}. Created {when(w.created_at)}{w.finished_at ? `, finished ${when(w.finished_at)}` : ""}.</p>
          {w.error && <p className="text-sm text-red-300">Error: {w.error}{w.failed_job_id ? <> (job <Link className="underline" to={`/jobs/j/${encodeURIComponent(w.failed_job_id)}`}>{w.failed_job_id}</Link>)</> : null}</p>}
          <Panel title="Graph"><WorkflowGraph steps={w.steps} /></Panel>
          <Panel title="Steps">
            <table aria-label="Workflow steps" className="w-full text-left text-sm">
              <thead className="text-xs uppercase tracking-wide text-zinc-500"><tr><th className="py-1">Step</th><th>Job</th><th>Status</th><th>Progress</th><th>Attempts</th></tr></thead>
              <tbody>
                {w.steps.map((s) => (
                  <tr key={s.job.id} className="border-t border-zinc-800 align-top">
                    <td className="py-1.5">{s.name ?? s.job.kind}{s.role === "compensation" && <span className="ml-1 text-xs text-amber-300">compensation</span>}</td>
                    <td><Link className="font-mono text-xs text-amber-300 hover:underline" to={`/jobs/j/${encodeURIComponent(s.job.id)}`}>{s.job.id}</Link></td>
                    <td><StatusBadge status={s.job.status} />{(s.job.waiting_for ?? 0) > 0 && <span className="ml-1 text-xs text-zinc-400">waiting for {s.job.waiting_for}</span>}</td>
                    <td className="w-64"><ProgressBar progress={s.job.progress} label={`Progress of ${s.name ?? s.job.id}`} /></td>
                    <td>{s.job.attempts}/{s.job.max_attempts}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </Panel>
        </>
      )}
    </div>
  );
}
