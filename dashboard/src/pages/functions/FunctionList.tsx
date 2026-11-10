import { useState } from "react";
import { Link, useNavigate } from "react-router-dom";
import { deleteFunction, listFunctions } from "../../api";
import { Button, ConfirmButton, ErrorView, Loading } from "../../components/ui";
import { useLoad } from "../../hooks";
import DeployForm from "./DeployForm";
import { when } from "./util";
import { Table } from "panelui-web/table";

export default function FunctionList() {
  const list = useLoad(listFunctions, "functions");
  const nav = useNavigate();
  const [error, setError] = useState<unknown>();
  const [deploying, setDeploying] = useState(false);

  async function remove(name: string) {
    setError(undefined);
    try {
      await deleteFunction(name);
      list.reload();
    } catch (e) {
      setError(e);
    }
  }

  return (
    <div className="space-y-4">
      <div className="flex items-center justify-between">
        <p className="text-sm text-muted-foreground">Sandboxed WebAssembly functions of your tenant.</p>
        <Button tone="primary" onClick={() => setDeploying((d) => !d)}>{deploying ? "Cancel" : "Deploy function"}</Button>
      </div>
      {deploying && <DeployForm onDone={(d) => nav(`/functions/f/${encodeURIComponent(d.function.name)}`)} />}
      <ErrorView error={error ?? list.error} />
      {list.loading && !list.error && <Loading what="Loading functions" />}
      {list.data && list.data.length === 0 && <p className="text-sm text-muted-foreground">No functions yet.</p>}
      {list.data && list.data.length > 0 && (
        <Table className="w-full text-left text-sm">
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
                <Table.Cell className="text-right"><ConfirmButton label={`Delete ${f.name}`} onConfirm={() => void remove(f.name)} /></Table.Cell>
              </Table.Row>
            ))}
          </Table.Body>
        </Table>
      )}
    </div>
  );
}
