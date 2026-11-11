import { useState } from "react";
import { Link } from "react-router-dom";
import { Input } from "panelui-web/input";
import { Table } from "panelui-web/table";
import { Textarea } from "panelui-web/textarea";
import { createNode, deleteNode, listNodes, updateNode } from "../../api";
import type { GraphNode } from "../../api";
import { ConfirmDialog } from "../../components/ConfirmDialog";
import { CreateSheet } from "../../components/CreateSheet";
import { EmptyState } from "../../components/EmptyState";
import { fieldErrors } from "../../components/ErrorPanel";
import { FormField } from "../../components/FormField";
import { Pager } from "../../components/Pager";
import { SkeletonTable } from "../../components/Skeleton";
import { Button, ErrorView } from "../../components/ui";
import { useCreateParam } from "../../hooks";
import { notify } from "../../toaster";
import { parseJsonObject } from "../vector/util";
import { usePaged } from "./usePaged";

const PAGE = 50;

export default function NodeBrowser() {
  const [label, setLabel] = useState("");
  const [applied, setApplied] = useState("");
  const [creating, setCreating] = useCreateParam();
  const [editing, setEditing] = useState<GraphNode>();
  const { items, next, more, loaded, error, reload, setError } = usePaged((after, s) => listNodes({ label: applied || undefined, after, limit: PAGE }, s), `nodes/${applied}`);

  async function remove(id: string) {
    await deleteNode(id);
    notify.deleted(`Node ${id}`);
    reload();
  }

  return (
    <div className="space-y-4">
      <form className="flex flex-wrap items-end gap-2" onSubmit={(e) => { e.preventDefault(); setApplied(label.trim()); }}>
        <FormField label="Label">{(p) => <Input {...p} size="sm" aria-label="Label filter" value={label} onChange={(e) => setLabel(e.target.value)} />}</FormField>
        <Button type="submit">Apply filter</Button>
        <Button type="button" tone="primary" className="ml-auto" onClick={() => setCreating(true)}>New node</Button>
      </form>
      <ErrorView error={error} onRetry={reload} />
      {!loaded && !error && <SkeletonTable rows={4} columns={4} label="Loading nodes" />}
      {loaded && items.length === 0 && (
        applied
          ? <EmptyState compact title="No nodes with this label" why="Change the label, or clear the filter to see every node." />
          : <EmptyState title="No nodes yet" why="A node is a thing in your graph, like a person or a product. Give it a label to group similar nodes, and properties to describe it." action={<Button tone="primary" onClick={() => setCreating(true)}>New node</Button>} docs="docs/adr/0006-graph.md" />
      )}
      {loaded && items.length > 0 && (
        <Table aria-label="Nodes" className="w-full text-left text-sm">
          <Table.Header className="text-xs uppercase tracking-wide text-muted-foreground"><Table.Row><Table.Head className="py-2">Id</Table.Head><Table.Head>Label</Table.Head><Table.Head>Properties</Table.Head><Table.Head><span className="sr-only">Actions</span></Table.Head></Table.Row></Table.Header>
          <Table.Body>
            {items.map((n) => (
              <Table.Row key={n.id} className="border-t border-border">
                <Table.Cell className="py-2 font-mono">{n.id}</Table.Cell>
                <Table.Cell>{n.label}</Table.Cell>
                <Table.Cell className="max-w-sm truncate font-mono text-xs text-muted-foreground">{JSON.stringify(n.properties)}</Table.Cell>
                <Table.Cell className="space-x-2 text-right">
                  <Link className="text-primary hover:underline" to={`/graph/explorer?node=${encodeURIComponent(n.id)}`}>Explore</Link>
                  <Button onClick={() => setEditing(n)}>Edit {n.id}</Button>
                  <ConfirmDialog
                    trigger={<Button tone="danger" aria-label={`Delete ${n.id}`}>Delete</Button>}
                    title={`Delete node ${n.id}?`}
                    consequence="The node and every edge that touches it are deleted."
                    confirmLabel="Delete node"
                    onConfirm={() => remove(n.id).catch((e: unknown) => setError(e))}
                  />
                </Table.Cell>
              </Table.Row>
            ))}
          </Table.Body>
        </Table>
      )}
      {loaded && items.length > 0 && <Pager mode="more" shown={items.length} hasMore={!!next} onMore={() => void more()} noun="nodes" label="More nodes" />}
      <NodeSheet open={creating} onOpenChange={setCreating} onDone={reload} />
      {editing && <NodeSheet key={editing.id} open node={editing} onOpenChange={(o) => !o && setEditing(undefined)} onDone={reload} />}
    </div>
  );
}

const EXAMPLE = '{\n  "name": "Alice",\n  "role": "engineer"\n}';

function NodeSheet({ open, onOpenChange, node, onDone }: { open: boolean; onOpenChange: (o: boolean) => void; node?: GraphNode; onDone: () => void }) {
  const [id, setId] = useState("");
  const [label, setLabel] = useState(node?.label ?? "");
  const [props, setProps] = useState(node ? JSON.stringify(node.properties, null, 2) : "");
  const [saveError, setSaveError] = useState<unknown>();
  let propsError: string | undefined;
  let properties: Record<string, unknown> = {};
  try { properties = parseJsonObject(props, "Properties") ?? {}; } catch (e) { propsError = (e as Error).message; }
  const body = node ? { label, properties } : { ...(id ? { id } : {}), ...(label ? { label } : {}), properties };
  const server = fieldErrors(saveError);
  return (
    <CreateSheet
      open={open}
      onOpenChange={onOpenChange}
      title={node ? `Edit node ${node.id}` : "New node"}
      description={node ? "Changes the label and replaces the properties." : "Adds a node to the graph."}
      submitLabel="Save node"
      canSubmit={!propsError}
      onError={setSaveError}
      onSubmit={async () => {
        if (node) {
          await updateNode(node.id, { label, properties });
          notify.saved(`Node ${node.id}`);
          onDone();
          return;
        }
        const n = await createNode(body);
        onDone();
        setId(""); setLabel(""); setProps(""); setSaveError(undefined);
        return { what: `Node ${n.id}` };
      }}
    >
      {!node && <FormField label="Id" help="Leave empty to generate one." error={server.id}>{(p) => <Input {...p} size="sm" value={id} onChange={(e) => setId(e.target.value)} />}</FormField>}
      <FormField label="Label" help="Groups similar nodes, such as person or product." error={server.label}>{(p) => <Input {...p} size="sm" value={label} onChange={(e) => setLabel(e.target.value)} />}</FormField>
      <FormField label="Properties" error={propsError} help="A JSON object that describes the node.">
        {(p) => <Textarea {...p} size="sm" autoResize={false} className="h-32 w-full font-mono text-xs" placeholder={EXAMPLE} value={props} onChange={(e) => setProps(e.target.value)} />}
      </FormField>
      <details className="rounded-md border border-border p-2 text-xs">
        <summary className="cursor-pointer font-medium">What this sends</summary>
        <p className="mt-2 text-muted-foreground"><code className="font-mono">{node ? `PUT /api/v1/graph/nodes/${node.id}` : "POST /api/v1/graph/nodes"}</code>, the same body your code sends:</p>
        <pre aria-label="Request body" className="mt-1 overflow-auto rounded bg-muted/60 p-2 font-mono">{JSON.stringify(body, null, 2)}</pre>
      </details>
    </CreateSheet>
  );
}
