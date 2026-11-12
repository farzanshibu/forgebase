/**
 * Extensions (FB-155): `/api/v1/system/extensions`. `managed_by` says who owns each one: `api` extensions are
 * installed and removed here; `schema` ones belong to ForgeBase's migrations and `feature:<name>` ones follow
 * Features and settings, so they are listed but not removable.
 */
import { useState } from "react";
import { Table } from "panelui-web/table";
import { createExtension, dropExtension, listExtensions, type ExtensionInfo } from "../../api";
import { ConfirmDialog } from "../../components/ConfirmDialog";
import { EmptyState } from "../../components/EmptyState";
import { SkeletonTable } from "../../components/Skeleton";
import { Button, ErrorView } from "../../components/ui";
import { useLoad } from "../../hooks";
import { notify } from "../../toaster";
import { managedWords } from "./pgHelpers";
import { Badge } from "panelui-web/badge";
import { TableCard } from "./parts";

const removable = (e: ExtensionInfo) => e.managed_by === "api" && !!e.installed_version;

export default function Extensions() {
  const { data, error, loading, reload } = useLoad(listExtensions, "pg-extensions");
  const [problem, setProblem] = useState<unknown>();
  async function install(name: string) {
    setProblem(undefined);
    try {
      await createExtension(name);
      notify.saved(`Extension ${name}`);
      reload();
    } catch (e) {
      setProblem(e);
    }
  }
  return (
    <div className="space-y-4">
      <p className="text-sm text-muted-foreground">PostgreSQL extensions add data types and functions. You can install the ones marked “You”; ForgeBase installs the others itself, and they follow Features and settings.</p>
      <ErrorView error={error} onRetry={reload} />
      <ErrorView error={problem} />
      {loading && !data && !error && <SkeletonTable rows={5} columns={6} label="Loading extensions" />}
      {data && data.length === 0 && <EmptyState compact title="No extensions listed" why="The server reported none for this PostgreSQL engine." />}
      {data && data.length > 0 && (
        <TableCard title="Extensions" count={data.length} description={`${data.filter((e) => e.installed_version).length} installed`}>
          <Table framed density="compact" aria-label="Extensions">
            <Table.Header className="bg-muted/40">
              <Table.Row><Table.Head>Extension</Table.Head><Table.Head>Managed by</Table.Head><Table.Head>Version</Table.Head><Table.Head>What it does</Table.Head><Table.Head align="end"><span className="sr-only">Actions</span></Table.Head></Table.Row>
            </Table.Header>
            <Table.Body>
              {data.map((e) => (
                <Table.Row key={e.name} className="align-top">
                  <Table.Cell className="py-2 font-mono text-xs">{e.name}</Table.Cell>
                  <Table.Cell className="py-2 text-xs">{managedWords(e.managed_by)}</Table.Cell>
                  <Table.Cell className="py-2 whitespace-nowrap">
                    {e.installed_version
                      ? <Badge variant="success" size="sm" dot>{e.installed_version} installed</Badge>
                      : <span className="text-xs text-muted-foreground">{e.available ? `${e.default_version ?? "?"} available` : "not in this engine"}</span>}
                  </Table.Cell>
                  <Table.Cell className="max-w-md py-2 text-xs text-muted-foreground">{e.comment ?? ""}</Table.Cell>
                  <Table.Cell align="end" className="py-2 whitespace-nowrap">
                    {e.managed_by === "api" && !e.installed_version && <Button disabled={!e.available} aria-label={`Install ${e.name}`} onClick={() => void install(e.name)}>Install</Button>}
                    {removable(e) && (
                      <ConfirmDialog
                        trigger={<Button tone="danger" aria-label={`Remove ${e.name}`}>Remove</Button>}
                        title={`Remove extension ${e.name}?`}
                        consequence="Its types and functions go away. PostgreSQL refuses while a table or function still uses it."
                        confirmLabel="Remove extension"
                        confirmText={e.name}
                        onConfirm={async () => { await dropExtension(e.name); notify.deleted(`Extension ${e.name}`); reload(); }}
                      />
                    )}
                    {e.managed_by !== "api" && <span className="text-xs text-muted-foreground" title="ForgeBase or Features and settings manage this one">managed</span>}
                  </Table.Cell>
                </Table.Row>
              ))}
            </Table.Body>
          </Table>
        </TableCard>
      )}
    </div>
  );
}
