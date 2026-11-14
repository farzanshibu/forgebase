import { useState } from "react";
import { Table } from "panelui-web/table";
import { alertHistory } from "../../api";
import type { Alert, AlertState } from "../../api";
import { EmptyState } from "../../components/EmptyState";
import { Pager } from "../../components/Pager";
import { Sel } from "../../components/controls";
import { SkeletonTable } from "../../components/Skeleton";
import { CardBody, HEAD_ROW, NUM, TableCard } from "../../components/TableCard";
import { Button, ErrorView } from "../../components/ui";
import { useCursorList } from "../../hooks";
import { When } from "../jobs/ops";
import AlertDetail from "./AlertDetail";
import { SeverityBadge, StateBadge } from "./shared";
import { fmtValue } from "./util";

/** Every alert, newest first, resolved ones included, read a page at a time (`next_before`). */
export default function History() {
  const [state, setState] = useState<"" | AlertState>("");
  const [open, setOpen] = useState<Alert>();
  const list = useCursorList<Alert>(
    (cursor, signal) => alertHistory({ state: state || undefined, before: cursor, limit: 50 }, signal).then((p) => ({ items: p.items, next: p.next_before })),
    `alert-history:${state}`,
  );
  return (
    <>
      <TableCard
        title="Alert history"
        count={list.items.length}
        description="Kept for the retention period set in alerts.history_retention_days."
        actions={<span className="flex gap-2"><Sel aria-label="State" value={state} onChange={setState} options={[{ value: "", label: "Any state" }, "firing", "pending", "resolved"]} /><Button onClick={list.reload}>Refresh</Button></span>}
        footer={list.items.length > 0 ? <Pager mode="more" shown={list.items.length} hasMore={list.hasMore} loading={list.loadingMore} onMore={list.loadMore} noun="alerts" /> : undefined}
      >
        <CardBody><ErrorView error={list.error} onRetry={list.reload} /></CardBody>
        {list.loading && <CardBody><SkeletonTable rows={4} columns={6} label="Loading history" /></CardBody>}
        {!list.loading && !list.error && list.items.length === 0 && <CardBody><EmptyState compact title="No alerts yet" why="When a rule's condition first holds, the alert is recorded here and stays after it resolves." /></CardBody>}
        {list.items.length > 0 && (
          <Table framed density="compact" aria-label="Alert history">
            <Table.Header className={HEAD_ROW}>
              <Table.Row><Table.Head>Alert</Table.Head><Table.Head>State</Table.Head><Table.Head>Severity</Table.Head><Table.Head className={NUM}>Value</Table.Head><Table.Head>Active</Table.Head><Table.Head>Resolved</Table.Head><Table.Head><span className="sr-only">Actions</span></Table.Head></Table.Row>
            </Table.Header>
            <Table.Body>
              {list.items.map((a) => (
                <Table.Row key={a.id}>
                  <Table.Cell><span className="font-medium">{a.name}</span><span className="block max-w-72 truncate text-xs text-muted-foreground">{Object.entries(a.labels).filter(([k]) => k !== "alertname" && k !== "severity").map(([k, v]) => `${k}=${v}`).join(" ")}</span></Table.Cell>
                  <Table.Cell><StateBadge state={a.state} /></Table.Cell>
                  <Table.Cell><SeverityBadge severity={a.severity} /></Table.Cell>
                  <Table.Cell className={NUM}>{fmtValue(a.value)}</Table.Cell>
                  <Table.Cell><When iso={a.active_at} /></Table.Cell>
                  <Table.Cell><When iso={a.resolved_at} fallback="—" /></Table.Cell>
                  <Table.Cell className="text-right"><Button aria-label={`Details of ${a.name} ${a.id}`} onClick={() => setOpen(a)}>Details</Button></Table.Cell>
                </Table.Row>
              ))}
            </Table.Body>
          </Table>
        )}
      </TableCard>
      {open && <AlertDetail alert={open} onClose={() => setOpen(undefined)} />}
    </>
  );
}
