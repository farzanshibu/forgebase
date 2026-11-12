import { Table } from "panelui-web/table";
import { listAdmins } from "../../api";
import { useOptionalSession } from "../../auth/session";
import { EmptyState } from "../../components/EmptyState";
import { ErrorPanel } from "../../components/ErrorPanel";
import { SkeletonTable } from "../../components/Skeleton";
import { useLoad } from "../../hooks";
import { Person } from "./common";
import { Badge } from "../../components/StatusBadge";
import { CardBody, HEAD_ROW, TableCard, Th } from "../../components/TableCard";
import { CopyIcon } from "../../components/ui";
import { IdText } from "../../components/IdText";
import { When } from "../../components/Time";

const CREATE = "forgebase admin create --email you@example.com";

export default function Admins() {
  const { data, error, loading, reload } = useLoad(listAdmins, "admins");
  const me = useOptionalSession()?.admin;
  return (
    <TableCard
      title="Administrators"
      count={data?.length}
      description="Administrators manage the whole server: every studio, every realm and every setting."
      footer={
        <p className="flex flex-wrap items-center gap-x-2 gap-y-1 text-xs text-muted-foreground">
          Add one on the server with
          <span className="inline-flex items-center gap-0.5 rounded-md border border-border bg-muted/60 ps-2">
            <code className="font-mono text-foreground/90">{CREATE}</code>
            <CopyIcon text={CREATE} label="Copy command" />
          </span>
        </p>
      }
    >
      {!!error && <CardBody><ErrorPanel error={error} onRetry={reload} /></CardBody>}
      {loading && !data && !error && <CardBody><SkeletonTable rows={2} columns={3} label="Loading administrators" /></CardBody>}
      {data?.length === 0 && <CardBody><EmptyState compact title="No administrators" why="Create one with forgebase admin create on the server." /></CardBody>}
      {!!data?.length && (
        <Table framed className="text-sm" aria-label="Administrators">
          <Table.Header className={HEAD_ROW}>
            <Table.Row><Th>Administrator</Th><Th>Created</Th><Th>Last sign-in</Th></Table.Row>
          </Table.Header>
          <Table.Body>
            {data.map((a) => (
              <Table.Row key={a.id}>
                <Table.Cell className="max-w-96">
                  <Person name={a.email} sub={<IdText id={a.id} what="administrator id" />}>
                    <span className="flex items-center gap-2">
                      <span className="truncate">{a.email}</span>
                      {me?.id === a.id && <Badge tone="info">You</Badge>}
                    </span>
                  </Person>
                </Table.Cell>
                <Table.Cell className="text-muted-foreground"><When value={a.created_at} /></Table.Cell>
                <Table.Cell className="text-muted-foreground"><When value={a.last_login_at} fallback="Never" /></Table.Cell>
              </Table.Row>
            ))}
          </Table.Body>
        </Table>
      )}
    </TableCard>
  );
}
