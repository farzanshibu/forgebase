import { Link } from "react-router-dom";
import { listCollections } from "../../api";
import { useLoad } from "../../hooks";
import { ErrorView, Loading } from "../../components/ui";
import { Table } from "panelui-web/table";

export default function CollectionBrowser() {
  const { data, error, loading } = useLoad(listCollections, "collections");
  return (
    <div>
      <div className="mb-4 flex items-center justify-between">
        <p className="text-sm text-muted-foreground">Every collection the server knows about.</p>
        <Link to="/database/new" className="rounded-md border border-primary bg-primary px-3 py-1.5 text-sm text-primary-foreground hover:bg-primary/90">
          New collection
        </Link>
      </div>
      <ErrorView error={error} />
      {loading && !error && <Loading what="Loading collections" />}
      {data && data.length === 0 && <p className="text-sm text-muted-foreground">No collections yet. Create one to start storing records.</p>}
      {data && data.length > 0 && (
        <Table className="w-full text-left text-sm">
          <Table.Header className="text-xs uppercase tracking-wide text-muted-foreground">
            <Table.Row>
              <Table.Head className="py-2">Name</Table.Head>
              <Table.Head>Type</Table.Head>
              <Table.Head>Mode</Table.Head>
              <Table.Head>Fields</Table.Head>
              <Table.Head>Version</Table.Head>
            </Table.Row>
          </Table.Header>
          <Table.Body>
            {data.map((c) => (
              <Table.Row key={c.name} className="border-t border-border">
                <Table.Cell className="py-2">
                  <Link className="text-primary hover:underline" to={`/database/c/${encodeURIComponent(c.name)}`}>
                    {c.name}
                  </Link>
                  {c.system && <span className="ml-2 text-xs text-muted-foreground">system</span>}
                </Table.Cell>
                <Table.Cell>{c.type}</Table.Cell>
                <Table.Cell>{c.schema_mode}</Table.Cell>
                <Table.Cell>{c.fields.length}</Table.Cell>
                <Table.Cell>{c.version}</Table.Cell>
              </Table.Row>
            ))}
          </Table.Body>
        </Table>
      )}
    </div>
  );
}
