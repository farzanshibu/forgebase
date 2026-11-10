import { listSchedules } from "../../api";
import { ErrorView, Loading } from "../../components/ui";
import { useLoad } from "../../hooks";
import { when } from "../functions/util";
import { Table } from "panelui-web/table";

export default function Schedules() {
  const list = useLoad(listSchedules, "job-schedules");
  return (
    <div className="space-y-3">
      <p className="text-sm text-muted-foreground">Recurring jobs (cron, UTC).</p>
      <ErrorView error={list.error} />
      {list.loading && !list.error && <Loading what="Loading schedules" />}
      {list.data && list.data.length === 0 && <p className="text-sm text-muted-foreground">No schedules.</p>}
      {list.data && list.data.length > 0 && (
        <Table aria-label="Schedules" className="w-full text-left text-sm">
          <Table.Header className="text-xs uppercase tracking-wide text-muted-foreground"><Table.Row><Table.Head className="py-2">Name</Table.Head><Table.Head>Cron</Table.Head><Table.Head>Queue</Table.Head><Table.Head>Kind</Table.Head><Table.Head>Enabled</Table.Head><Table.Head>Next run</Table.Head><Table.Head>Last run</Table.Head></Table.Row></Table.Header>
          <Table.Body>
            {list.data.map((s) => (
              <Table.Row key={s.id} className="border-t border-border">
                <Table.Cell className="py-2">{s.name}</Table.Cell><Table.Cell className="font-mono text-xs">{s.cron}</Table.Cell><Table.Cell>{s.queue}</Table.Cell><Table.Cell>{s.kind}</Table.Cell>
                <Table.Cell>{s.enabled ? "yes" : "no"}</Table.Cell><Table.Cell className="text-muted-foreground">{when(s.next_run_at)}</Table.Cell><Table.Cell className="text-muted-foreground">{when(s.last_run_at)}</Table.Cell>
              </Table.Row>
            ))}
          </Table.Body>
        </Table>
      )}
    </div>
  );
}
