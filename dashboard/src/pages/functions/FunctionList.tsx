import { useState } from "react";
import { Link, useNavigate } from "react-router-dom";
import { Badge } from "panelui-web/badge";
import { Table } from "panelui-web/table";
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
import { AccessBadge } from "./parts";
import { when } from "./util";

export default function FunctionList() {
  const list = useLoad(listFunctions, "functions");
  const nav = useNavigate();
  const [uploading, setUploading] = useState(false);
  const upload = <Button onClick={() => setUploading((d) => !d)}>{uploading ? "Cancel upload" : "Upload .wasm"}</Button>;
  const n = list.data?.length ?? 0;

  return (
    <div className="space-y-6">
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
        <section className="overflow-hidden rounded-xl border border-border bg-card">
          <header className="flex items-center justify-between gap-3 border-b border-border px-4 py-3">
            <h2 className="text-sm font-medium">All functions</h2>
            <span className="text-xs tabular-nums text-muted-foreground">{n} {n === 1 ? "function" : "functions"}</span>
          </header>
          <Table aria-label="Functions" framed density="compact" className="w-full text-left text-sm">
            <Table.Header className="text-xs text-muted-foreground">
              <Table.Row>
                <Table.Head>Name</Table.Head>
                <Table.Head align="end">Active</Table.Head>
                <Table.Head align="end">Latest</Table.Head>
                <Table.Head>HTTP access</Table.Head>
                <Table.Head>Updated</Table.Head>
                <Table.Head><span className="sr-only">Actions</span></Table.Head>
              </Table.Row>
            </Table.Header>
            <Table.Body>
              {list.data.map((f) => (
                <Table.Row key={f.name} className="border-t border-border hover:bg-muted/40">
                  <Table.Cell className="max-w-md py-2.5">
                    <Link className="font-mono font-medium text-primary hover:underline" to={`/functions/f/${encodeURIComponent(f.name)}`}>{f.name}</Link>
                    {f.description && <span className="block truncate text-xs text-muted-foreground" title={f.description}>{f.description}</span>}
                  </Table.Cell>
                  <Table.Cell align="end"><Badge variant="success" size="sm" className="font-mono">v{f.active_version}</Badge></Table.Cell>
                  <Table.Cell align="end" className={`font-mono ${f.latest_version !== f.active_version ? "text-warning" : "text-muted-foreground"}`} title={f.latest_version !== f.active_version ? "A newer version exists but is not active" : undefined}>v{f.latest_version}</Table.Cell>
                  <Table.Cell><AccessBadge access={f.access} /></Table.Cell>
                  <Table.Cell className="whitespace-nowrap font-mono text-xs text-muted-foreground">{when(f.updated_at)}</Table.Cell>
                  <Table.Cell align="end">
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
        </section>
      )}
    </div>
  );
}
