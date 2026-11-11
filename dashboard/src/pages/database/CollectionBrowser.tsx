import { Link, useNavigate } from "react-router-dom";
import { Table } from "panelui-web/table";
import { listCollections } from "../../api";
import { EmptyState } from "../../components/EmptyState";
import { SkeletonTable } from "../../components/Skeleton";
import { Button, ErrorView } from "../../components/ui";
import { useLoad } from "../../hooks";

export default function CollectionBrowser() {
  const nav = useNavigate();
  const { data, error, loading, reload } = useLoad(listCollections, "collections");
  return (
    <div>
      <ErrorView error={error} onRetry={reload} />
      {loading && !error && <SkeletonTable rows={4} columns={5} label="Loading collections" />}
      {data && data.length === 0 && (
        <EmptyState
          title="No collections yet"
          why="A collection stores records that share a schema, like a table of products or orders. Create one, then add records and read them from your app."
          action={<Button tone="primary" onClick={() => nav("/database/new")}>New collection</Button>}
          docs="docs/database.md"
        />
      )}
      {data && data.length > 0 && (
        <Table className="w-full text-left text-sm" aria-label="Collections">
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
