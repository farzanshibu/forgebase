import { useState } from "react";
import { Link } from "react-router-dom";
import { listWorkflows } from "../../api";
import type { WorkflowStatus } from "../../api";
import { Button, ErrorView, Loading, Panel, inputCls } from "../../components/ui";
import { useLoad } from "../../hooks";
import { when } from "../functions/util";
import { Table } from "panelui-web/table";

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
      {wf.data && wf.data.items.length === 0 && <p className="text-sm text-muted-foreground">No workflows match.</p>}
      {wf.data && wf.data.items.length > 0 && (
        <Table aria-label="Workflows" className="w-full text-left text-sm">
          <Table.Header className="text-xs uppercase tracking-wide text-muted-foreground"><Table.Row><Table.Head className="py-1">Workflow</Table.Head><Table.Head>Status</Table.Head><Table.Head>On failure</Table.Head><Table.Head>Created</Table.Head><Table.Head>Finished</Table.Head></Table.Row></Table.Header>
          <Table.Body>
            {wf.data.items.map((w) => (
              <Table.Row key={w.id} className="border-t border-border">
                <Table.Cell className="py-1.5"><Link className="text-primary hover:underline" to={`/jobs/w/${encodeURIComponent(w.id)}`}>{w.name}</Link> <span className="font-mono text-xs text-muted-foreground">{w.id}</span></Table.Cell>
                <Table.Cell>{w.status}</Table.Cell><Table.Cell>{w.on_failure}</Table.Cell><Table.Cell className="text-muted-foreground">{when(w.created_at)}</Table.Cell><Table.Cell className="text-muted-foreground">{when(w.finished_at)}</Table.Cell>
              </Table.Row>
            ))}
          </Table.Body>
        </Table>
      )}
    </Panel>
  );
}
