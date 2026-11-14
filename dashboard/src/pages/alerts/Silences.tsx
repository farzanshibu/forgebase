import { useState } from "react";
import { Table } from "panelui-web/table";
import { expireSilence, listSilences } from "../../api";
import { Check } from "../../components/controls";
import { ConfirmDialog } from "../../components/ConfirmDialog";
import { EmptyState } from "../../components/EmptyState";
import { SkeletonTable } from "../../components/Skeleton";
import { Badge } from "../../components/StatusBadge";
import { Chip } from "../../components/Chips";
import { CardBody, HEAD_ROW, TableCard } from "../../components/TableCard";
import { Button, ErrorView } from "../../components/ui";
import { useCreateParam, useLoad } from "../../hooks";
import { notify } from "../../toaster";
import { When } from "../jobs/ops";
import SilenceSheet from "./SilenceSheet";

/** Silences: who muted what, until when. A silence can be ended now; ended ones stay listed with "Show ended". */
export default function Silences() {
  const [creating, setCreating] = useCreateParam();
  const [all, setAll] = useState(false);
  const list = useLoad((s) => listSilences(all, s), `silences:${all}`);
  const items = list.data ?? [];
  return (
    <>
      <TableCard
        title="Silences"
        count={list.data?.length}
        description="A silence mutes the notifications of alerts whose labels match every matcher. The alerts still fire and stay listed."
        actions={<span className="flex items-center gap-3"><Check checked={all} onChange={setAll} label="Show ended" /><Button onClick={list.reload}>Refresh</Button></span>}
      >
        <CardBody><ErrorView error={list.error} onRetry={list.reload} /></CardBody>
        {list.loading && !list.data && <CardBody><SkeletonTable rows={3} columns={5} label="Loading silences" /></CardBody>}
        {list.data && items.length === 0 && <CardBody><EmptyState compact title="No silences" why="Silence an alert from the Alerts tab, or create one here for maintenance you know about." action={<Button tone="primary" onClick={() => setCreating(true)}>New silence</Button>} /></CardBody>}
        {items.length > 0 && (
          <Table framed density="compact" aria-label="Silences">
            <Table.Header className={HEAD_ROW}>
              <Table.Row><Table.Head>Matchers</Table.Head><Table.Head>Status</Table.Head><Table.Head>Ends</Table.Head><Table.Head>Comment</Table.Head><Table.Head><span className="sr-only">Actions</span></Table.Head></Table.Row>
            </Table.Header>
            <Table.Body>
              {items.map((s) => (
                <Table.Row key={s.id}>
                  <Table.Cell className="py-2"><ul aria-label="Matchers" className="flex flex-wrap gap-1">{s.matchers.map((m, i) => <li key={i}><Chip>{m.label}{m.op}{m.value}</Chip></li>)}</ul></Table.Cell>
                  <Table.Cell><Badge status={s.status}>{s.status}</Badge></Table.Cell>
                  <Table.Cell><When iso={s.ends_at} /></Table.Cell>
                  <Table.Cell className="max-w-64 truncate text-muted-foreground" title={s.comment}>{s.comment || "—"}<span className="block text-xs">by {s.created_by}</span></Table.Cell>
                  <Table.Cell className="text-right">
                    {s.status !== "expired" && (
                      <ConfirmDialog
                        trigger={<Button tone="danger" aria-label={`End silence ${s.id}`}>End now</Button>}
                        title="End this silence?"
                        consequence="Matching alerts that are firing start sending notifications again at the next evaluation."
                        confirmLabel="End silence"
                        tone="warning"
                        onConfirm={async () => { await expireSilence(s.id); notify.saved("Silence ended"); list.reload(); }}
                      />
                    )}
                  </Table.Cell>
                </Table.Row>
              ))}
            </Table.Body>
          </Table>
        )}
      </TableCard>
      {creating && <SilenceSheet open onOpenChange={setCreating} onCreated={list.reload} />}
    </>
  );
}
