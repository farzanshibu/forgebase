import { useState } from "react";
import { Link } from "react-router-dom";
import { listWorkflows } from "../../api";
import type { WorkflowStatus } from "../../api";
import { Button, ErrorView, Loading, Panel, inputCls } from "../../components/ui";
import { useLoad } from "../../hooks";
import { when } from "../functions/util";

const STATUSES: WorkflowStatus[] = ["running", "succeeded", "failed", "compensating", "compensated", "cancelled"];

export default function WorkflowList() {
  const [status, setStatus] = useState<WorkflowStatus | "">("");
  const wf = useLoad((s) => listWorkflows({ status: status || undefined, limit: 50 }, s), `workflows:${status}`);
  return (
    <Panel title="Workflows" actions={<Button onClick={wf.reload}>Refresh</Button>}>
      <label className="mb-3 block text-sm">Status
        <select className={`${inputCls} ml-2`} value={status} onChange={(e) => setStatus(e.target.value as WorkflowStatus | "")}>
          <option value="">all</option>
          {STATUSES.map((s) => <option key={s} value={s}>{s}</option>)}
        </select>
      </label>
      <ErrorView error={wf.error} />
      {wf.loading && !wf.error && <Loading what="Loading workflows" />}
      {wf.data && wf.data.items.length === 0 && <p className="text-sm text-zinc-400">No workflows match.</p>}
      {wf.data && wf.data.items.length > 0 && (
        <table aria-label="Workflows" className="w-full text-left text-sm">
          <thead className="text-xs uppercase tracking-wide text-zinc-500"><tr><th className="py-1">Workflow</th><th>Status</th><th>On failure</th><th>Created</th><th>Finished</th></tr></thead>
          <tbody>
            {wf.data.items.map((w) => (
              <tr key={w.id} className="border-t border-zinc-800">
                <td className="py-1.5"><Link className="text-amber-300 hover:underline" to={`/jobs/w/${encodeURIComponent(w.id)}`}>{w.name}</Link> <span className="font-mono text-xs text-zinc-500">{w.id}</span></td>
                <td>{w.status}</td><td>{w.on_failure}</td><td className="text-zinc-400">{when(w.created_at)}</td><td className="text-zinc-400">{when(w.finished_at)}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </Panel>
  );
}
