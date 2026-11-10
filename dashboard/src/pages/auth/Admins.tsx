import { listAdmins } from "../../api";
import { ErrorView, Loading } from "../../components/ui";
import { useLoad } from "../../hooks";
import { THEAD, Th, fmtTime } from "./common";
import { Table } from "panelui-web/table";

export default function Admins() {
  const { data, error, loading } = useLoad(listAdmins, "admins");
  return (
    <div>
      <ErrorView error={error} />
      {loading && !error && <Loading what="Loading administrators" />}
      {data?.length === 0 && <p className="text-sm text-muted-foreground">No administrators.</p>}
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
