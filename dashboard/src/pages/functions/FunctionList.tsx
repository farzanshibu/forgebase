import { useState } from "react";
import { Link, useNavigate } from "react-router-dom";
import { deleteFunction, listFunctions } from "../../api";
import { ConfirmDialog } from "../../components/ConfirmDialog";
import { EmptyState } from "../../components/EmptyState";
import { ErrorPanel } from "../../components/ErrorPanel";
import { PageHeader } from "../../components/PageHeader";
import { SkeletonTable } from "../../components/Skeleton";
import { Button } from "../../components/ui";
import { useLoad } from "../../hooks";
import { notify } from "../../toaster";
import DeployForm from "./DeployForm";
import { when } from "./util";
import { Table } from "panelui-web/table";

export default function FunctionList() {
  const list = useLoad(listFunctions, "functions");
  const nav = useNavigate();
  const [uploading, setUploading] = useState(false);
  const upload = <Button onClick={() => setUploading((d) => !d)}>{uploading ? "Cancel upload" : "Upload .wasm"}</Button>;

  return (
    <div className="space-y-4">
      <PageHeader studio="functions" secondary={upload} primary={<Button tone="primary" onClick={() => nav("/functions/new")}>New function</Button>} />
      {uploading && <DeployForm onDone={(d) => nav(`/functions/f/${encodeURIComponent(d.function.name)}`)} />}
      <ErrorPanel error={list.error} onRetry={list.reload} />
      {list.loading && !list.error && <SkeletonTable rows={3} columns={6} label="Loading functions" />}
      {list.data && list.data.length === 0 && (
        <EmptyState
          title="No functions yet"
          why="A function is your own code that the server runs next to your data, when a request, a record change or a schedule calls it. Start from a template: no toolchain needed."
          action={<Button tone="primary" onClick={() => nav("/functions/new")}>New function</Button>}
          sample={upload}
          docs="docs/adr/0017-functions.md"
        />
      )}
      {list.data && list.data.length > 0 && (
        <Table aria-label="Functions" className="w-full text-left text-sm">
          <Table.Header className="text-xs uppercase tracking-wide text-muted-foreground">
            <Table.Row><Table.Head className="py-2">Name</Table.Head><Table.Head>Active</Table.Head><Table.Head>Latest</Table.Head><Table.Head>HTTP access</Table.Head><Table.Head>Updated</Table.Head><Table.Head /></Table.Row>
          </Table.Header>
          <Table.Body>
            {list.data.map((f) => (
              <Table.Row key={f.name} className="border-t border-border">
                <Table.Cell className="py-2">
                  <Link className="text-primary hover:underline" to={`/functions/f/${encodeURIComponent(f.name)}`}>{f.name}</Link>
                  {f.description && <span className="ml-2 text-muted-foreground">{f.description}</span>}
                </Table.Cell>
                <Table.Cell>v{f.active_version}</Table.Cell>
                <Table.Cell>v{f.latest_version}</Table.Cell>
                <Table.Cell>{f.access}</Table.Cell>
                <Table.Cell className="text-muted-foreground">{when(f.updated_at)}</Table.Cell>
                <Table.Cell className="text-right">
                  <ConfirmDialog
                    trigger={<Button tone="danger" aria-label={`Delete ${f.name}`}>Delete</Button>}
                    title={`Delete function ${f.name}?`}
                    consequence="Every version, its secrets and its logs are deleted. Apps and hooks that call it get an error."
                    confirmLabel="Delete function"
                    confirmText={f.name}
                    onConfirm={async () => { await deleteFunction(f.name); notify.deleted(`Function ${f.name}`); list.reload(); }}
                  />
                </Table.Cell>
              </Table.Row>
            ))}
          </Table.Body>
        </Table>
      )}
    </div>
  );
}
