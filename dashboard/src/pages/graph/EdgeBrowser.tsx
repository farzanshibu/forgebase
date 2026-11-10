import { useState } from "react";
import { createEdge, deleteEdge, listEdges, updateEdge } from "../../api";
import type { GraphEdge } from "../../api";
import { Button, ConfirmButton, ErrorView, Loading, Panel, inputCls } from "../../components/ui";
import { parseJsonObject } from "../vector/util";
import { usePaged } from "./usePaged";
import { Table } from "panelui-web/table";
import { Input } from "panelui-web/input";

const PAGE = 50;
interface Filters { type: string; source: string; target: string }

export default function EdgeBrowser() {
  const [f, setF] = useState<Filters>({ type: "", source: "", target: "" });
  const [applied, setApplied] = useState<Filters>(f);
  const [editing, setEditing] = useState<GraphEdge | "new">();
  const { items, next, more, loaded, error, reload, setError } = usePaged(
    (after, s) => listEdges({ type: applied.type, source: applied.source, target: applied.target, after, limit: PAGE }, s),
    `edges/${JSON.stringify(applied)}`,
  );
  const refresh = reload;
  async function remove(id: string) {
    try {
      await deleteEdge(id);
      refresh();
    } catch (e) {
      setError(e);
    }
  }
  const field = (k: keyof Filters, label: string) => (
    <label className="text-sm">{label}<Input size="sm" aria-label={`${label} filter`} className="mt-1 block" value={f[k]} onChange={(e) => setF({ ...f, [k]: e.target.value })} /></label>
  );

  return (
    <div className="space-y-4">
      <div className="flex flex-wrap items-end gap-2">
        {field("type", "Type")}{field("source", "Source")}{field("target", "Target")}
        <Button onClick={() => setApplied(f)}>Apply filter</Button>
        <Button tone="primary" onClick={() => setEditing("new")}>New edge</Button>
      </div>
      <ErrorView error={error} />
      {!loaded && !error && <Loading what="Loading edges" />}
      {loaded && (
        <Table className="w-full text-left text-sm">
          <Table.Header className="text-xs uppercase tracking-wide text-muted-foreground"><Table.Row><Table.Head className="py-2">Id</Table.Head><Table.Head>Type</Table.Head><Table.Head>Source</Table.Head><Table.Head>Target</Table.Head><Table.Head>Properties</Table.Head><Table.Head /></Table.Row></Table.Header>
          <Table.Body>
            {items.map((e) => (
              <Table.Row key={e.id} className="border-t border-border">
                <Table.Cell className="py-2 font-mono">{e.id}</Table.Cell><Table.Cell>{e.type}</Table.Cell>
                <Table.Cell className="font-mono">{e.source}</Table.Cell><Table.Cell className="font-mono">{e.target}{e.directed ? "" : " (undirected)"}</Table.Cell>
                <Table.Cell className="max-w-xs truncate font-mono text-xs text-muted-foreground">{JSON.stringify(e.properties)}</Table.Cell>
                <Table.Cell className="space-x-2 text-right">
                  <Button onClick={() => setEditing(e)}>Edit {e.id}</Button>
                  <ConfirmButton label={`Delete ${e.id}`} onConfirm={() => void remove(e.id)} />
                </Table.Cell>
              </Table.Row>
            ))}
            {items.length === 0 && <Table.Row><Table.Cell colSpan={6} className="py-3 text-muted-foreground">No edges.</Table.Cell></Table.Row>}
          </Table.Body>
        </Table>
      )}
      {next && <Button onClick={() => void more()}>Load more</Button>}
      {editing && <EdgeForm key={editing === "new" ? "new" : editing.id} edge={editing === "new" ? undefined : editing} onDone={() => { setEditing(undefined); refresh(); }} />}
    </div>
  );
}

function EdgeForm({ edge, onDone }: { edge?: GraphEdge; onDone: () => void }) {
  const [type, setType] = useState("");
  const [source, setSource] = useState("");
  const [target, setTarget] = useState("");
  const [directed, setDirected] = useState(true);
  const [props, setProps] = useState(JSON.stringify(edge?.properties ?? {}, null, 2));
  const [error, setError] = useState<unknown>();

  async function save(e: React.FormEvent) {
    e.preventDefault();
    setError(undefined);
    try {
      const properties = parseJsonObject(props, "Properties") ?? {};
      if (edge) await updateEdge(edge.id, properties);
      else await createEdge({ type, source, target, directed, properties });
      onDone();
    } catch (err) {
      setError(err);
    }
  }

  return (
    <Panel title={edge ? `Edit edge ${edge.id}` : "New edge"}>
      <form aria-label={edge ? "Edit edge" : "New edge"} onSubmit={(e) => void save(e)} className="space-y-3">
        {!edge && (
          <div className="grid gap-3 md:grid-cols-3">
            <label className="text-sm">Type<Input size="sm" className="mt-1 w-full" value={type} onChange={(e) => setType(e.target.value)} required /></label>
            <label className="text-sm">Source node id<Input size="sm" className="mt-1 w-full" value={source} onChange={(e) => setSource(e.target.value)} required /></label>
            <label className="text-sm">Target node id<Input size="sm" className="mt-1 w-full" value={target} onChange={(e) => setTarget(e.target.value)} required /></label>
            <label className="flex items-center gap-2 text-sm"><input type="checkbox" checked={directed} onChange={(e) => setDirected(e.target.checked)} />Directed</label>
          </div>
        )}
        <label className="block text-sm">Properties (JSON)<textarea className={`${inputCls} mt-1 h-28 w-full font-mono`} value={props} onChange={(e) => setProps(e.target.value)} /></label>
        <div className="flex gap-2"><Button tone="primary" type="submit">Save edge</Button><Button onClick={onDone}>Cancel</Button></div>
      </form>
      <div className="mt-3"><ErrorView error={error} /></div>
    </Panel>
  );
}
