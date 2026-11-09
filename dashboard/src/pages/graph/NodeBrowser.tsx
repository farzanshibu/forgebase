import { useState } from "react";
import { Link } from "react-router-dom";
import { createNode, deleteNode, listNodes, updateNode } from "../../api";
import type { GraphNode } from "../../api";
import { Button, ConfirmButton, ErrorView, Loading, Panel, inputCls } from "../../components/ui";
import { parseJsonObject } from "../vector/util";
import { usePaged } from "./usePaged";

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
        <label className="text-sm">Label<input aria-label="Label filter" className={`${inputCls} mt-1 block`} value={label} onChange={(e) => setLabel(e.target.value)} /></label>
        <Button onClick={apply}>Apply filter</Button>
        <Button tone="primary" onClick={() => setEditing("new")}>New node</Button>
      </div>
      <ErrorView error={error} />
      {!loaded && !error && <Loading what="Loading nodes" />}
      {loaded && (
        <table className="w-full text-left text-sm">
          <thead className="text-xs uppercase tracking-wide text-zinc-500"><tr><th className="py-2">Id</th><th>Label</th><th>Properties</th><th /></tr></thead>
          <tbody>
            {items.map((n) => (
              <tr key={n.id} className="border-t border-zinc-800">
                <td className="py-2 font-mono">{n.id}</td>
                <td>{n.label}</td>
                <td className="max-w-sm truncate font-mono text-xs text-zinc-400">{JSON.stringify(n.properties)}</td>
                <td className="space-x-2 text-right">
                  <Link className="text-amber-300 hover:underline" to={`/graph/explorer?node=${encodeURIComponent(n.id)}`}>Explore</Link>
                  <Button onClick={() => setEditing(n)}>Edit {n.id}</Button>
                  <ConfirmButton label={`Delete ${n.id}`} onConfirm={() => void remove(n.id)} />
                </td>
              </tr>
            ))}
            {items.length === 0 && <tr><td colSpan={4} className="py-3 text-zinc-400">No nodes.</td></tr>}
          </tbody>
        </table>
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
        {!node && <label className="block text-sm">Id (blank = generated)<input className={`${inputCls} mt-1 w-full`} value={id} onChange={(e) => setId(e.target.value)} /></label>}
        <label className="block text-sm">Label<input className={`${inputCls} mt-1 w-full`} value={label} onChange={(e) => setLabel(e.target.value)} /></label>
        <label className="block text-sm">Properties (JSON)<textarea className={`${inputCls} mt-1 h-32 w-full font-mono`} value={props} onChange={(e) => setProps(e.target.value)} /></label>
        <div className="flex gap-2"><Button tone="primary" type="submit">Save node</Button><Button onClick={onDone}>Cancel</Button></div>
      </form>
      <div className="mt-3"><ErrorView error={error} /></div>
    </Panel>
  );
}
