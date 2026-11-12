import { listSchedules } from "../../api";
import { Button } from "../../components/ui";
import { EmptyState } from "../../components/EmptyState";
import { ErrorPanel } from "../../components/ErrorPanel";
import { HelpTip } from "../../components/HelpTip";
import { SkeletonTable } from "../../components/Skeleton";
import { useLoad } from "../../hooks";
import Tenant from "./Tenant";
import { CardBody, HEAD_ROW, TableCard } from "../../components/TableCard";
import { Dot, When } from "./ops";
import { Link } from "react-router-dom";
import { Table } from "panelui-web/table";

export default function Schedules() {
  const list = useLoad(listSchedules, "job-schedules");
  return (
    <TableCard
      title="Schedules"
      count={list.data?.length}
      description={<span className="inline-flex flex-wrap items-center gap-1">Recurring jobs. Each one runs on a cron schedule <HelpTip term="cron" /> in UTC.</span>}
      actions={<Button onClick={list.reload}>Refresh</Button>}
    >
      <CardBody className="space-y-3">
        <ErrorPanel error={list.error} onRetry={list.reload} />
        {list.loading && !list.data && !list.error && <SkeletonTable rows={3} columns={8} label="Loading schedules" />}
        {list.data && list.data.length === 0 && (
          <EmptyState
            title="No schedules yet"
            why={<>A schedule enqueues a job again and again on a cron expression: five fields, minute, hour, day of month, month and weekday. <code className="font-mono">0 3 * * *</code> runs at 03:00 UTC every day; <code className="font-mono">*/15 * * * *</code> runs every 15 minutes. Create one with POST /api/v1/jobs/schedules, or from a function (see the Scheduled job template).</>}
            docs="docs/adr/0007-jobs.md"
          />
        )}
      </CardBody>
      {list.data && list.data.length > 0 && (
          <Table framed density="compact" aria-label="Schedules">
            <Table.Header className={HEAD_ROW}><Table.Row><Table.Head className="py-2">Name</Table.Head><Table.Head>Cron</Table.Head><Table.Head>Kind</Table.Head><Table.Head>Queue</Table.Head><Table.Head>Tenant</Table.Head><Table.Head>State</Table.Head><Table.Head className="text-right">Next run</Table.Head><Table.Head className="text-right">Last run</Table.Head></Table.Row></Table.Header>
            <Table.Body>
              {list.data.map((s) => (
                <Table.Row key={s.id}>
                  <Table.Cell className="py-2 font-medium">{s.name}{s.created_by ? <span className="block text-xs font-normal text-muted-foreground">by {s.created_by}</span> : null}</Table.Cell>
                  <Table.Cell><code className="whitespace-nowrap rounded bg-muted px-1.5 py-0.5 font-mono text-xs">{s.cron}</code></Table.Cell>
                  <Table.Cell className="font-mono text-xs">{s.kind}</Table.Cell>
                  <Table.Cell className="text-muted-foreground">{s.queue}</Table.Cell>
                  <Table.Cell><Tenant value={s.tenant} /></Table.Cell>
                  <Table.Cell>
                    <span className={`inline-flex items-center gap-1.5 whitespace-nowrap ${s.enabled ? "" : "text-muted-foreground"}`}><Dot tone={s.enabled ? "ok" : "neutral"} />{s.enabled ? "Enabled" : "Paused"}</span>
                  </Table.Cell>
                  <Table.Cell className="text-right">{s.enabled ? <When iso={s.next_run_at} /> : <span className="text-muted-foreground">—</span>}</Table.Cell>
                  <Table.Cell className="text-right text-muted-foreground">
                    {s.last_job_id && s.last_run_at ? <Link to={`/jobs/j/${encodeURIComponent(s.last_job_id)}`} className="hover:text-foreground hover:underline" title="Open the job it last enqueued"><When iso={s.last_run_at} /></Link> : <When iso={s.last_run_at} />}
                  </Table.Cell>
                </Table.Row>
              ))}
            </Table.Body>
          </Table>
      )}
    </TableCard>
  );
}
