import { listSchedules } from "../../api";
import { EmptyState } from "../../components/EmptyState";
import { ErrorPanel } from "../../components/ErrorPanel";
import { HelpTip } from "../../components/HelpTip";
import { SkeletonTable } from "../../components/Skeleton";
import { useLoad } from "../../hooks";
import { when } from "../functions/util";
import Tenant from "./Tenant";
import { Table } from "panelui-web/table";

export default function Schedules() {
  const list = useLoad(listSchedules, "job-schedules");
  return (
    <div className="space-y-3">
      <p className="flex items-center gap-1 text-sm text-muted-foreground">Recurring jobs. Each one runs on a cron schedule <HelpTip term="cron" /> in UTC.</p>
      <ErrorPanel error={list.error} onRetry={list.reload} />
      {list.loading && !list.error && <SkeletonTable rows={3} columns={8} label="Loading schedules" />}
      {list.data && list.data.length === 0 && (
        <EmptyState
          title="No schedules yet"
          why={<>A schedule enqueues a job again and again on a cron expression: five fields, minute, hour, day of month, month and weekday. <code className="font-mono">0 3 * * *</code> runs at 03:00 UTC every day; <code className="font-mono">*/15 * * * *</code> runs every 15 minutes. Create one with POST /api/v1/jobs/schedules, or from a function (see the Scheduled job template).</>}
          docs="docs/adr/0007-jobs.md"
        />
      )}
      {list.data && list.data.length > 0 && (
        <Table aria-label="Schedules" className="w-full text-left text-sm">
          <Table.Header className="text-xs uppercase tracking-wide text-muted-foreground"><Table.Row><Table.Head className="py-2">Name</Table.Head><Table.Head>Cron</Table.Head><Table.Head>Queue</Table.Head><Table.Head>Kind</Table.Head><Table.Head>Tenant</Table.Head><Table.Head>Enabled</Table.Head><Table.Head>Next run</Table.Head><Table.Head>Last run</Table.Head></Table.Row></Table.Header>
          <Table.Body>
            {list.data.map((s) => (
              <Table.Row key={s.id} className="border-t border-border">
                <Table.Cell className="py-2">{s.name}{s.created_by ? <span className="block text-xs text-muted-foreground">by {s.created_by}</span> : null}</Table.Cell><Table.Cell className="font-mono text-xs">{s.cron}</Table.Cell><Table.Cell>{s.queue}</Table.Cell><Table.Cell>{s.kind}</Table.Cell>
                <Table.Cell><Tenant value={s.tenant} /></Table.Cell>
                <Table.Cell>{s.enabled ? "yes" : "no"}</Table.Cell><Table.Cell className="text-muted-foreground">{when(s.next_run_at)}</Table.Cell><Table.Cell className="text-muted-foreground">{when(s.last_run_at)}</Table.Cell>
              </Table.Row>
            ))}
          </Table.Body>
        </Table>
      )}
    </div>
  );
}
