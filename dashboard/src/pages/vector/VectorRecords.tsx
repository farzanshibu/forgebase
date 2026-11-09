import { useState } from "react";
import { deleteVectorItem, getVectorItem, upsertVectors, vectorSearch } from "../../api";
import type { VectorCollection, VectorItem } from "../../api";
import { Button, ConfirmButton, ErrorView, Loading, Panel, inputCls } from "../../components/ui";
import { useLoad } from "../../hooks";
import { parseJsonObject, parseVector } from "./util";

const BROWSE_LIMIT = 1000;

/**
 * The vector API has no list route, so records are browsed with an exact search for every
 * item (`exact: true`, `k` = 1000, a unit probe vector); order is by distance to that probe.
 */
export default function VectorRecords({ collection, onChanged }: { collection: VectorCollection; onChanged: () => void }) {
  const field = collection.fields[0];
  const { data, error, loading, reload } = useLoad(async () => {
    if (!field) return [];
    const r = await vectorSearch(collection.name, {
      field: field.name, vector: Array.from({ length: field.dimension }, () => 1), k: BROWSE_LIMIT, exact: true, include_metadata: true, include_text: true,
    });
    return r.hits;
  }, `vrecords/${collection.name}/${collection.items}`);
  const [selected, setSelected] = useState<string>();
  const [fail, setFail] = useState<unknown>();

  async function remove(id: string) {
    setFail(undefined);
    try {
      await deleteVectorItem(collection.name, id);
      if (selected === id) setSelected(undefined);
      reload();
      onChanged();
    } catch (e) {
      setFail(e);
    }
  }

  if (!field) return <p className="text-sm text-zinc-400">This collection has no vector field yet; create one with a field to store records.</p>;
  return (
    <div className="space-y-4">
      <ErrorView error={fail ?? error} />
      {loading && !error && <Loading what="Loading records" />}
      {data && (
        <div>
          <p className="mb-2 text-sm text-zinc-400">
            {data.length} records{data.length >= BROWSE_LIMIT ? ` (first ${BROWSE_LIMIT} shown)` : ""}
          </p>
          <table className="w-full text-left text-sm">
            <thead className="text-xs uppercase tracking-wide text-zinc-500">
              <tr><th className="py-2">Id</th><th>Metadata</th><th>Text</th><th /></tr>
            </thead>
            <tbody>
              {data.map((h) => (
                <tr key={h.id} className="border-t border-zinc-800">
                  <td className="py-2"><button type="button" className="font-mono text-amber-300 hover:underline" onClick={() => setSelected(h.id)}>{h.id}</button></td>
                  <td className="max-w-xs truncate font-mono text-xs text-zinc-400">{JSON.stringify(h.metadata ?? {})}</td>
                  <td className="max-w-xs truncate text-zinc-400">{h.text ?? ""}</td>
                  <td className="text-right"><ConfirmButton label={`Delete ${h.id}`} onConfirm={() => void remove(h.id)} /></td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
      {selected && <ItemDetail key={selected} name={collection.name} id={selected} />}
      <UpsertForm collection={collection} onDone={() => { reload(); onChanged(); }} />
    </div>
  );
}

function ItemDetail({ name, id }: { name: string; id: string }) {
  const { data, error, loading } = useLoad<VectorItem>((s) => getVectorItem(name, id, s), `vitem/${name}/${id}`);
  return (
    <Panel title={`Item ${id}`}>
      <ErrorView error={error} />
      {loading && !error && <Loading what="Loading item" />}
      {data && <pre aria-label="Item JSON" className="max-h-64 overflow-auto text-xs text-zinc-300">{JSON.stringify(data, null, 2)}</pre>}
    </Panel>
  );
}

function UpsertForm({ collection, onDone }: { collection: VectorCollection; onDone: () => void }) {
  const [id, setId] = useState("");
  const [field, setField] = useState(collection.fields[0]?.name ?? "");
  const [vector, setVector] = useState("");
  const [metadata, setMetadata] = useState("");
  const [text, setText] = useState("");
  const [error, setError] = useState<unknown>();
  const [result, setResult] = useState<string>();

  async function submit(e: React.FormEvent) {
    e.preventDefault();
    setError(undefined);
    setResult(undefined);
    try {
      const item = { ...(id ? { id } : {}), vectors: { [field]: parseVector(vector) }, metadata: parseJsonObject(metadata, "Metadata"), ...(text ? { text } : {}) };
      const r = await upsertVectors(collection.name, [item]);
      setResult(`Saved ${r.ids.join(", ")} (${r.created} created, ${r.updated} updated)`);
      onDone();
    } catch (err) {
      setError(err);
    }
  }

  return (
    <Panel title="Upsert record">
      <form aria-label="Upsert record" onSubmit={(e) => void submit(e)} className="grid gap-3 md:grid-cols-2">
        <label className="text-sm">Id (blank = generated)<input className={`${inputCls} mt-1 w-full`} value={id} onChange={(e) => setId(e.target.value)} /></label>
        <label className="text-sm">Field
          <select className={`${inputCls} mt-1 w-full`} value={field} onChange={(e) => setField(e.target.value)}>
            {collection.fields.map((f) => <option key={f.name} value={f.name}>{f.name} ({f.dimension})</option>)}
          </select>
        </label>
        <label className="text-sm md:col-span-2">Vector<input className={`${inputCls} mt-1 w-full font-mono`} value={vector} onChange={(e) => setVector(e.target.value)} placeholder="0.1, 0.2, 0.3" /></label>
        <label className="text-sm">Metadata (JSON object)<input className={`${inputCls} mt-1 w-full font-mono`} value={metadata} onChange={(e) => setMetadata(e.target.value)} placeholder='{"tag":"a"}' /></label>
        <label className="text-sm">Text (for hybrid search)<input className={`${inputCls} mt-1 w-full`} value={text} onChange={(e) => setText(e.target.value)} /></label>
        <div><Button tone="primary" type="submit">Upsert</Button></div>
      </form>
      {result && <p role="status" className="mt-3 text-sm text-emerald-300">{result}</p>}
      <div className="mt-3"><ErrorView error={error} /></div>
    </Panel>
  );
}
