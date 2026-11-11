import { useState } from "react";
import { deleteFunctionSecret, listFunctionSecrets, putFunctionSecret } from "../../api";
import { Button, ErrorView, Panel } from "../../components/ui";
import { ConfirmDialog } from "../../components/ConfirmDialog";
import { EmptyState } from "../../components/EmptyState";
import { HelpTip } from "../../components/HelpTip";
import { SkeletonTable } from "../../components/Skeleton";
import { notify } from "../../toaster";
import { useLoad } from "../../hooks";
import { when } from "./util";
import { Table } from "panelui-web/table";
import { Input } from "panelui-web/input";

/** Secret names only: the API never returns a value, and the form clears it as soon as it is stored. */
export default function SecretsTab({ name }: { name: string }) {
  const secrets = useLoad((s) => listFunctionSecrets(name, s), `secrets:${name}`);
  const [key, setKey] = useState("");
  const [value, setValue] = useState("");
  const [error, setError] = useState<unknown>();
  const [busy, setBusy] = useState(false);

  async function save(e: React.FormEvent) {
    e.preventDefault();
    setBusy(true);
    setError(undefined);
    try {
      await putFunctionSecret(name, key, value);
      notify.saved(`Secret ${key}`);
      setKey("");
      setValue("");
      secrets.reload();
    } catch (err) {
      setError(err);
    } finally {
      setBusy(false);
    }
  }

  async function remove(k: string) {
    setError(undefined);
    await deleteFunctionSecret(name, k);
    notify.deleted(`Secret ${k}`);
    secrets.reload();
  }

  return (
    <div className="space-y-4">
      <Panel title="Set a secret">
        <p className="mb-2 flex items-center gap-1 text-xs text-muted-foreground">What is a secret? <HelpTip term="secret" /> The function reads it as an environment value; needs the secrets capability.</p>
        <form aria-label="Set secret" onSubmit={(e) => void save(e)} className="grid gap-3 md:grid-cols-3">
          <label className="text-sm">Name<Input size="sm" className="mt-1 w-full" value={key} onChange={(e) => setKey(e.target.value)} required /></label>
          <label className="text-sm">Value
            <Input size="sm" type="password" autoComplete="off" className="mt-1 w-full" value={value} onChange={(e) => setValue(e.target.value)} required />
          </label>
          <div className="flex items-end"><Button tone="primary" type="submit" disabled={busy}>Save secret</Button></div>
        </form>
        <p className="mt-2 text-xs text-muted-foreground">Values are sealed at rest and never shown again. Setting an existing name replaces it.</p>
      </Panel>
      <ErrorView error={error ?? secrets.error} />
      {secrets.loading && !secrets.error && <SkeletonTable rows={2} columns={4} label="Loading secrets" />}
      {secrets.data && secrets.data.length === 0 && <EmptyState compact title="No secrets yet" why="Store an API key or a signing secret here instead of writing it in the code. The function reads it with ctx.env.get(NAME)." />}
      {secrets.data && secrets.data.length > 0 && (
        <Table className="w-full text-left text-sm">
          <Table.Header className="text-xs uppercase tracking-wide text-muted-foreground"><Table.Row><Table.Head className="py-2">Name</Table.Head><Table.Head>Created</Table.Head><Table.Head>Updated</Table.Head><Table.Head /></Table.Row></Table.Header>
          <Table.Body>
            {secrets.data.map((s) => (
              <Table.Row key={s.name} className="border-t border-border">
                <Table.Cell className="py-2 font-mono">{s.name}</Table.Cell>
                <Table.Cell className="text-muted-foreground">{when(s.created_at)}</Table.Cell>
                <Table.Cell className="text-muted-foreground">{when(s.updated_at)}</Table.Cell>
                <Table.Cell className="text-right"><ConfirmDialog trigger={<Button tone="danger" aria-label={`Delete ${s.name}`}>Delete</Button>} title={`Delete secret ${s.name}?`} consequence="Functions that read it get nothing until you set it again." confirmLabel="Delete secret" confirmText={s.name} onConfirm={() => remove(s.name)} /></Table.Cell>
              </Table.Row>
            ))}
          </Table.Body>
        </Table>
      )}
    </div>
  );
}
