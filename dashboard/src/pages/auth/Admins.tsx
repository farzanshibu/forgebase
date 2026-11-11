import { listAdmins } from "../../api";
import { EmptyState } from "../../components/EmptyState";
import { ErrorPanel } from "../../components/ErrorPanel";
import { SkeletonTable } from "../../components/Skeleton";
import { useLoad } from "../../hooks";
import { THEAD, Th, fmtTime } from "./common";
import { Table } from "panelui-web/table";

export default function Admins() {
  const { data, error, loading } = useLoad(listAdmins, "admins");
  return (
    <div>
      <p className="mb-3 max-w-2xl text-sm text-muted-foreground">Administrators manage the whole server. Add one with <code>forgebase admin create --email you@example.com</code> on the server.</p>
      <ErrorPanel error={error} />
      {loading && !error && <SkeletonTable rows={2} columns={3} label="Loading administrators" />}
      {data?.length === 0 && <EmptyState compact title="No administrators" why="Create one with forgebase admin create on the server." />}
      {!!data?.length && (
        <Table className="w-full text-sm">
          <Table.Header className={THEAD}>
            <Table.Row><Th>Email</Th><Th>Created</Th><Th>Last sign-in</Th></Table.Row>
          </Table.Header>
          <Table.Body>
            {data.map((a) => (
              <Table.Row key={a.id} className="border-t border-border">
                <Table.Cell className="py-2 pr-3">{a.email}</Table.Cell>
                <Table.Cell className="pr-3">{fmtTime(a.created_at)}</Table.Cell>
                <Table.Cell className="pr-3">{fmtTime(a.last_login_at)}</Table.Cell>
              </Table.Row>
            ))}
          </Table.Body>
        </Table>
      )}
    </div>
  );
}
