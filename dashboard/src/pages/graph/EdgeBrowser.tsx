import { useState } from "react";
import { Input } from "panelui-web/input";
import { Table } from "panelui-web/table";
import { Textarea } from "panelui-web/textarea";
import { createEdge, deleteEdge, listEdges, updateEdge } from "../../api";
import type { GraphEdge } from "../../api";
import { Check } from "../../components/controls";
import { ConfirmDialog } from "../../components/ConfirmDialog";
import { CreateSheet } from "../../components/CreateSheet";
import { EmptyState } from "../../components/EmptyState";
import { fieldErrors } from "../../components/ErrorPanel";
import { FormField } from "../../components/FormField";
import { Pager } from "../../components/Pager";
import { SkeletonTable } from "../../components/Skeleton";
import { Button, ErrorView } from "../../components/ui";
import { notify } from "../../toaster";
import { parseJsonObject } from "../vector/util";
import { usePaged } from "./usePaged";

const PAGE = 50;
interface Filters { type: string; source: string; target: string }

export default function EdgeBrowser() {
  const [f, setF] = useState<Filters>({ type: "", source: "", target: "" });
  const [applied, setApplied] = useState<Filters>(f);
  const [creating, setCreating] = useState(false);
  const [editing, setEditing] = useState<GraphEdge>();
  const { items, next, more, loaded, error, reload, setError } = usePaged(
    (after, s) => listEdges({ type: applied.type, source: applied.source, target: applied.target, after, limit: PAGE }, s),
    `edges/${JSON.stringify(applied)}`,
  );
  async function remove(id: string) {
    await deleteEdge(id);
    notify.deleted(`Edge ${id}`);
    reload();
  }
  const field = (k: keyof Filters, label: string) => (
    <FormField label={label}>{(p) => <Input {...p} size="sm" aria-label={`${label} filter`} value={f[k]} onChange={(e) => setF({ ...f, [k]: e.target.value })} />}</FormField>
  );
  const filtered = !!(applied.type || applied.source || applied.target);

  return (
    <div className="space-y-4">
      <form className="flex flex-wrap items-end gap-2" onSubmit={(e) => { e.preventDefault(); setApplied(f); }}>
        {field("type", "Type")}{field("source", "Source")}{field("target", "Target")}
        <Button type="submit">Apply filter</Button>
        <Button type="button" tone="primary" className="ml-auto" onClick={() => setCreating(true)}>New edge</Button>
      </form>
      <ErrorView error={error} onRetry={reload} />
      {!loaded && !error && <SkeletonTable rows={4} columns={6} label="Loading edges" />}
      {loaded && items.length === 0 && (
        filtered
          ? <EmptyState compact title="No edges match" why="Change the filters, or clear them to see every edge." />
          : <EmptyState title="No edges yet" why="An edge is a relationship between two nodes, like “alice knows bob”. Create nodes first, then connect them here." action={<Button tone="primary" onClick={() => setCreating(true)}>New edge</Button>} docs="docs/adr/0006-graph.md" />
      )}
      {loaded && items.length > 0 && (
        <Table aria-label="Edges" className="w-full text-left text-sm">
          <Table.Header className="text-xs uppercase tracking-wide text-muted-foreground"><Table.Row><Table.Head className="py-2">Id</Table.Head><Table.Head>Type</Table.Head><Table.Head>Source</Table.Head><Table.Head>Target</Table.Head><Table.Head>Properties</Table.Head><Table.Head><span className="sr-only">Actions</span></Table.Head></Table.Row></Table.Header>
          <Table.Body>
            {items.map((e) => (
              <Table.Row key={e.id} className="border-t border-border">
                <Table.Cell className="py-2 font-mono">{e.id}</Table.Cell><Table.Cell>{e.type}</Table.Cell>
                <Table.Cell className="font-mono">{e.source}</Table.Cell><Table.Cell className="font-mono">{e.target}{e.directed ? "" : " (undirected)"}</Table.Cell>
                <Table.Cell className="max-w-xs truncate font-mono text-xs text-muted-foreground">{JSON.stringify(e.properties)}</Table.Cell>
                <Table.Cell className="space-x-2 text-right">
                  <Button onClick={() => setEditing(e)}>Edit {e.id}</Button>
                  <ConfirmDialog
                    trigger={<Button tone="danger" aria-label={`Delete ${e.id}`}>Delete</Button>}
                    title={`Delete edge ${e.id}?`}
                    consequence="The relationship is removed. Both nodes stay."
                    confirmLabel="Delete edge"
                    onConfirm={() => remove(e.id).catch((err: unknown) => setError(err))}
                  />
                </Table.Cell>
              </Table.Row>
            ))}
          </Table.Body>
        </Table>
      )}
      {loaded && items.length > 0 && <Pager mode="more" shown={items.length} hasMore={!!next} onMore={() => void more()} noun="edges" label="More edges" />}
      <EdgeSheet open={creating} onOpenChange={setCreating} onDone={reload} />
      {editing && <EdgeSheet key={editing.id} open edge={editing} onOpenChange={(o) => !o && setEditing(undefined)} onDone={reload} />}
    </div>
  );
}

const EXAMPLE = '{\n  "since": 2024\n}';

function EdgeSheet({ open, onOpenChange, edge, onDone }: { open: boolean; onOpenChange: (o: boolean) => void; edge?: GraphEdge; onDone: () => void }) {
  const [type, setType] = useState("");
  const [source, setSource] = useState("");
  const [target, setTarget] = useState("");
  const [directed, setDirected] = useState(true);
  const [props, setProps] = useState(edge ? JSON.stringify(edge.properties, null, 2) : "");
  const [saveError, setSaveError] = useState<unknown>();
  let propsError: string | undefined;
  let properties: Record<string, unknown> = {};
  try { properties = parseJsonObject(props, "Properties") ?? {}; } catch (e) { propsError = (e as Error).message; }
  const body = edge ? { properties } : { type, source, target, directed, properties };
  const server = fieldErrors(saveError);
  return (
    <CreateSheet
      open={open}
      onOpenChange={onOpenChange}
      title={edge ? `Edit edge ${edge.id}` : "New edge"}
      description={edge ? "Replaces the properties. The type and the two nodes cannot change." : "Connects two existing nodes."}
      submitLabel="Save edge"
      canSubmit={!propsError && (!!edge || (!!type && !!source && !!target))}
      onError={setSaveError}
      onSubmit={async () => {
        if (edge) {
          await updateEdge(edge.id, properties);
          notify.saved(`Edge ${edge.id}`);
          onDone();
          return;
        }
        const e = await createEdge({ type, source, target, directed, properties });
        onDone();
        setType(""); setSource(""); setTarget(""); setProps(""); setSaveError(undefined);
        return { what: `Edge ${e.id}` };
      }}
    >
      {!edge && (
        <>
          <FormField label="Type" required help="The kind of relationship, such as knows or bought." error={server.type}>{(p) => <Input {...p} size="sm" value={type} onChange={(e) => setType(e.target.value)} />}</FormField>
          <FormField label="Source node id" required error={server.source}>{(p) => <Input {...p} size="sm" className="font-mono" value={source} onChange={(e) => setSource(e.target.value)} />}</FormField>
          <FormField label="Target node id" required error={server.target}>{(p) => <Input {...p} size="sm" className="font-mono" value={target} onChange={(e) => setTarget(e.target.value)} />}</FormField>
          <Check checked={directed} onChange={setDirected} label="Directed" description="The edge points from the source to the target. Turn off for a two-way relationship." />
        </>
      )}
      <FormField label="Properties" error={propsError} help="A JSON object that describes the relationship.">
        {(p) => <Textarea {...p} size="sm" autoResize={false} className="h-28 w-full font-mono text-xs" placeholder={EXAMPLE} value={props} onChange={(e) => setProps(e.target.value)} />}
      </FormField>
      <details className="rounded-md border border-border p-2 text-xs">
        <summary className="cursor-pointer font-medium">What this sends</summary>
        <p className="mt-2 text-muted-foreground"><code className="font-mono">{edge ? `PATCH /api/v1/graph/edges/${edge.id}` : "POST /api/v1/graph/edges"}</code>, the same body your code sends:</p>
        <pre aria-label="Request body" className="mt-1 overflow-auto rounded bg-muted/60 p-2 font-mono">{JSON.stringify(body, null, 2)}</pre>
      </details>
    </CreateSheet>
  );
}
