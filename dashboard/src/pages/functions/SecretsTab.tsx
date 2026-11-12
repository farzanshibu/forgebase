import { useState } from "react";
import { deleteFunctionSecret, listFunctionSecrets, putFunctionSecret } from "../../api";
import { Button, ErrorView } from "../../components/ui";
import { FormField } from "../../components/FormField";
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

  const n = secrets.data?.length ?? 0;
  return (
    <div className="grid items-start gap-4 lg:grid-cols-[minmax(0,1fr)_22rem]">
      <section aria-labelledby="fn-secrets" className="overflow-hidden rounded-xl border border-border bg-card lg:order-1">
        <header className="flex items-center justify-between gap-3 border-b border-border px-4 py-3">
          <h2 id="fn-secrets" className="flex items-center gap-1 text-sm font-medium">Secrets <HelpTip term="secret" /></h2>
          {secrets.data && <span className="text-xs tabular-nums text-muted-foreground">{n} {n === 1 ? "secret" : "secrets"}</span>}
        </header>
        <div className={secrets.data && n > 0 ? "" : "p-4"}>
          <ErrorView error={secrets.error} />
          {secrets.loading && !secrets.error && <SkeletonTable rows={2} columns={4} label="Loading secrets" />}
          {secrets.data && n === 0 && <EmptyState compact title="No secrets yet" why="Store an API key or a signing secret here instead of writing it in the code. The function reads it with ctx.env.get(NAME)." />}
          {secrets.data && n > 0 && (
            <Table framed density="compact" className="w-full text-left text-sm">
              <Table.Header className="text-xs text-muted-foreground"><Table.Row><Table.Head>Name</Table.Head><Table.Head>Value</Table.Head><Table.Head>Created</Table.Head><Table.Head>Updated</Table.Head><Table.Head><span className="sr-only">Actions</span></Table.Head></Table.Row></Table.Header>
              <Table.Body>
                {secrets.data.map((s) => (
                  <Table.Row key={s.name} className="border-t border-border hover:bg-muted/40">
                    <Table.Cell className="py-2.5 font-mono font-medium">{s.name}</Table.Cell>
                    <Table.Cell className="font-mono text-xs text-muted-foreground" title="Values are never shown again">••••••••</Table.Cell>
                    <Table.Cell className="whitespace-nowrap font-mono text-xs text-muted-foreground">{when(s.created_at)}</Table.Cell>
                    <Table.Cell className="whitespace-nowrap font-mono text-xs text-muted-foreground">{when(s.updated_at)}</Table.Cell>
                    <Table.Cell align="end"><ConfirmDialog trigger={<Button tone="danger" aria-label={`Delete ${s.name}`}>Delete</Button>} title={`Delete secret ${s.name}?`} consequence="Functions that read it get nothing until you set it again." confirmLabel="Delete secret" confirmText={s.name} onConfirm={() => remove(s.name)} /></Table.Cell>
                  </Table.Row>
                ))}
              </Table.Body>
            </Table>
          )}
        </div>
      </section>
      <section aria-labelledby="fn-secret-set" className="rounded-xl border border-border bg-card p-4 lg:order-2">
        <h2 id="fn-secret-set" className="text-sm font-medium">Set a secret</h2>
        <p className="mt-1 text-xs text-muted-foreground">The function reads it with <code className="font-mono">ctx.env.get(NAME)</code> and needs the <span className="font-mono">secrets</span> capability. Setting an existing name replaces it.</p>
        <form aria-label="Set secret" onSubmit={(e) => void save(e)} className="mt-3 space-y-3">
          <FormField label="Name">{(p) => <Input {...p} size="sm" className="w-full font-mono" value={key} onChange={(e) => setKey(e.target.value)} placeholder="API_KEY" required autoComplete="off" />}</FormField>
          <FormField label="Value" help="Sealed at rest and never shown again.">
            {(p) => <Input {...p} size="sm" type="password" autoComplete="off" className="w-full" value={value} onChange={(e) => setValue(e.target.value)} required />}
          </FormField>
          <ErrorView error={error} />
          <Button tone="primary" type="submit" disabled={busy || !key.trim() || !value}>{busy ? "Saving…" : "Save secret"}</Button>
        </form>
      </section>
    </div>
  );
}
