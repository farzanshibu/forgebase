import { useState } from "react";
import { Link } from "react-router-dom";
import { createNode, deleteNode, listNodes, updateNode } from "../../api";
import type { GraphNode } from "../../api";
import { Button, ConfirmButton, ErrorView, Loading, Panel } from "../../components/ui";
import { parseJsonObject } from "../vector/util";
import { usePaged } from "./usePaged";
import { Table } from "panelui-web/table";
import { Input } from "panelui-web/input";
import { Textarea } from "panelui-web/textarea";

const PAGE = 50;

export default function NodeBrowser() {
  const [label, setLabel] = useState("");
  const [applied, setApplied] = useState("");
  const [editing, setEditing] = useState<GraphNode | "new">();
  const { items, next, more, loaded, error, reload, setError } = usePaged((after, s) => listNodes({ label: applied || undefined, after, limit: PAGE }, s), `nodes/${applied}`);
  const apply = () => setApplied(label);
  const refresh = reload;

  async function remove(id: string) {
    try {
      await deleteNode(id);
      refresh();
    } catch (e) {
      setError(e);
    }
  }

  return (
    <div className="space-y-4">
      <div className="flex flex-wrap items-end gap-2">
        <label className="text-sm">Label<Input size="sm" aria-label="Label filter" className="mt-1 block" value={label} onChange={(e) => setLabel(e.target.value)} /></label>
        <Button onClick={apply}>Apply filter</Button>
        <Button tone="primary" onClick={() => setEditing("new")}>New node</Button>
      </div>
      <ErrorView error={error} />
      {!loaded && !error && <Loading what="Loading nodes" />}
      {loaded && (
        <Table className="w-full text-left text-sm">
          <Table.Header className="text-xs uppercase tracking-wide text-muted-foreground"><Table.Row><Table.Head className="py-2">Id</Table.Head><Table.Head>Label</Table.Head><Table.Head>Properties</Table.Head><Table.Head /></Table.Row></Table.Header>
          <Table.Body>
            {items.map((n) => (
              <Table.Row key={n.id} className="border-t border-border">
                <Table.Cell className="py-2 font-mono">{n.id}</Table.Cell>
                <Table.Cell>{n.label}</Table.Cell>
                <Table.Cell className="max-w-sm truncate font-mono text-xs text-muted-foreground">{JSON.stringify(n.properties)}</Table.Cell>
                <Table.Cell className="space-x-2 text-right">
                  <Link className="text-primary hover:underline" to={`/graph/explorer?node=${encodeURIComponent(n.id)}`}>Explore</Link>
                  <Button onClick={() => setEditing(n)}>Edit {n.id}</Button>
                  <ConfirmButton label={`Delete ${n.id}`} onConfirm={() => void remove(n.id)} />
                </Table.Cell>
              </Table.Row>
            ))}
            {items.length === 0 && <Table.Row><Table.Cell colSpan={4} className="py-3 text-muted-foreground">No nodes.</Table.Cell></Table.Row>}
          </Table.Body>
        </Table>
      )}
      {next && <Button onClick={() => void more()}>Load more</Button>}
      {editing && <NodeForm key={editing === "new" ? "new" : editing.id} node={editing === "new" ? undefined : editing} onDone={() => { setEditing(undefined); refresh(); }} />}
    </div>
  );
}

function NodeForm({ node, onDone }: { node?: GraphNode; onDone: () => void }) {
  const [id, setId] = useState("");
  const [label, setLabel] = useState(node?.label ?? "");
  const [props, setProps] = useState(JSON.stringify(node?.properties ?? {}, null, 2));
  const [error, setError] = useState<unknown>();

  async function save(e: React.FormEvent) {
    e.preventDefault();
    setError(undefined);
    try {
      const properties = parseJsonObject(props, "Properties") ?? {};
      if (node) await updateNode(node.id, { label, properties });
      else await createNode({ ...(id ? { id } : {}), ...(label ? { label } : {}), properties });
      onDone();
    } catch (err) {
      setError(err);
    }
  }

  return (
    <Panel title={node ? `Edit node ${node.id}` : "New node"}>
      <form aria-label={node ? "Edit node" : "New node"} onSubmit={(e) => void save(e)} className="space-y-3">
        {!node && <label className="block text-sm">Id (blank = generated)<Input size="sm" className="mt-1 w-full" value={id} onChange={(e) => setId(e.target.value)} /></label>}
        <label className="block text-sm">Label<Input size="sm" className="mt-1 w-full" value={label} onChange={(e) => setLabel(e.target.value)} /></label>
        <label className="block text-sm">Properties (JSON)<Textarea size="sm" autoResize={false} className="mt-1 h-32 w-full font-mono" value={props} onChange={(e) => setProps(e.target.value)} /></label>
        <div className="flex gap-2"><Button tone="primary" type="submit">Save node</Button><Button onClick={onDone}>Cancel</Button></div>
      </form>
      <div className="mt-3"><ErrorView error={error} /></div>
    </Panel>
  );
}
