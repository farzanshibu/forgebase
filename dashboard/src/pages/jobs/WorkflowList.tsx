import { useState } from "react";
import { Link } from "react-router-dom";
import { listWorkflows } from "../../api";
import type { WorkflowStatus } from "../../api";
import { Button } from "../../components/ui";
import { EmptyState } from "../../components/EmptyState";
import { ErrorPanel } from "../../components/ErrorPanel";
import { HelpTip } from "../../components/HelpTip";
import { SkeletonTable } from "../../components/Skeleton";
import Tenant from "./Tenant";
import { useLoad } from "../../hooks";
import { WorkflowStatusBadge } from "./StatusBadge";
import { CardBody, HEAD_ROW, TableCard } from "../../components/TableCard";
import { When } from "./ops";
import { Table } from "panelui-web/table";
import { Sel } from "../../components/controls";

const STATUSES: WorkflowStatus[] = ["running", "succeeded", "failed", "compensating", "compensated", "cancelled"];
const cap = (s: string) => s.charAt(0).toUpperCase() + s.slice(1);

export default function WorkflowList() {
  const [status, setStatus] = useState<WorkflowStatus | "">("");
  const wf = useLoad((s) => listWorkflows({ status: status || undefined, limit: 50 }, s), `workflows:${status}`);
  return (
    <TableCard
      title="Workflows"
      count={wf.data?.items.length}
      actions={
        <>
          <Sel aria-label="Status" className="w-40" value={status} onChange={(v) => setStatus(v as WorkflowStatus | "")} options={[{ value: "", label: "All statuses" }, ...STATUSES.map((s) => ({ value: s, label: cap(s) }))]} />
          <Button onClick={wf.reload}>Refresh</Button>
        </>
      }
    >
      <CardBody className="space-y-3">
        <ErrorPanel error={wf.error} onRetry={wf.reload} />
        {wf.loading && !wf.data && !wf.error && <SkeletonTable rows={3} columns={6} label="Loading workflows" />}
        {wf.data && wf.data.items.length === 0 && (
          <EmptyState compact
            title={status ? "No workflows match" : "No workflows yet"}
            why={status ? "Choose another status to see more." : <span className="inline-flex flex-wrap items-center justify-center gap-1">A workflow joins jobs into steps: each starts when the steps it depends on succeed. <HelpTip term="workflow" /> Create one with POST /api/v1/jobs/workflows.</span>} />
        )}
      </CardBody>
      {wf.data && wf.data.items.length > 0 && (
          <Table framed density="compact" aria-label="Workflows">
            <Table.Header className={HEAD_ROW}><Table.Row><Table.Head className="py-1">Workflow</Table.Head><Table.Head>Status</Table.Head><Table.Head>On failure</Table.Head><Table.Head>Tenant</Table.Head><Table.Head className="text-right">Created</Table.Head><Table.Head className="text-right">Finished</Table.Head></Table.Row></Table.Header>
            <Table.Body>
              {wf.data.items.map((w) => (
                <Table.Row key={w.id}>
                  <Table.Cell className="py-2">
                    <Link className="font-medium text-primary hover:underline" to={`/jobs/w/${encodeURIComponent(w.id)}`}>{w.name}</Link>
                    <span className="block max-w-56 truncate font-mono text-xs text-muted-foreground" title={w.id}>{w.id}</span>
                  </Table.Cell>
                  <Table.Cell><WorkflowStatusBadge status={w.status} /></Table.Cell>
                  <Table.Cell className="text-muted-foreground">{w.on_failure === "compensate" ? "Undo finished steps" : "Stop"}</Table.Cell>
                  <Table.Cell><Tenant value={w.tenant} /></Table.Cell>
                  <Table.Cell className="text-right text-muted-foreground"><When iso={w.created_at} /></Table.Cell>
                  <Table.Cell className="text-right text-muted-foreground"><When iso={w.finished_at} empty="—" /></Table.Cell>
                </Table.Row>
              ))}
            </Table.Body>
          </Table>
      )}
    </TableCard>
  );
}
