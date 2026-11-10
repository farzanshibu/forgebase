import { useState } from "react";
import { Link } from "react-router-dom";
import { createVectorCollection, deleteVectorCollection, listVectorCollections, vectorStats } from "../../api";
import type { Metric, Quantization } from "../../api";
import { Button, ConfirmButton, ErrorView, Loading, Panel, inputCls } from "../../components/ui";
import { useLoad } from "../../hooks";
import { bytes } from "./util";
import { Table } from "panelui-web/table";
import { Input } from "panelui-web/input";

export default function VectorCollections() {
  const list = useLoad(listVectorCollections, "vector-collections");
  const stats = useLoad(vectorStats, "vector-stats");
  const [error, setError] = useState<unknown>();
  const [creating, setCreating] = useState(false);

  async function remove(name: string) {
    setError(undefined);
    try {
      await deleteVectorCollection(name);
      list.reload();
      stats.reload();
    } catch (e) {
      setError(e);
    }
  }

  return (
    <div className="space-y-4">
      {stats.data && (
        <p aria-label="Vector totals" className="text-sm text-muted-foreground">
          {stats.data.collections} collections · {stats.data.indexes} indexes ({stats.data.building} building) · {stats.data.indexed_vectors} indexed vectors · {bytes(stats.data.memory_bytes)} index memory
        </p>
      )}
      <div className="flex items-center justify-between">
        <p className="text-sm text-muted-foreground">Vector collections of your tenant.</p>
        <Button tone="primary" onClick={() => setCreating((c) => !c)}>
          {creating ? "Cancel" : "New collection"}
        </Button>
      </div>
      {creating && (
        <CreateForm
          onDone={() => {
            setCreating(false);
            list.reload();
            stats.reload();
          }}
        />
      )}
      <ErrorView error={error ?? list.error} />
      {list.loading && !list.error && <Loading what="Loading collections" />}
      {list.data && list.data.length === 0 && <p className="text-sm text-muted-foreground">No vector collections yet.</p>}
      {list.data && list.data.length > 0 && (
        <Table className="w-full text-left text-sm">
          <Table.Header className="text-xs uppercase tracking-wide text-muted-foreground">
            <Table.Row>
              <Table.Head className="py-2">Name</Table.Head>
              <Table.Head>Fields</Table.Head>
              <Table.Head>Items</Table.Head>
              <Table.Head>Description</Table.Head>
              <Table.Head />
            </Table.Row>
          </Table.Header>
          <Table.Body>
            {list.data.map((c) => (
              <Table.Row key={c.name} className="border-t border-border">
                <Table.Cell className="py-2">
                  <Link className="text-primary hover:underline" to={`/vector/c/${encodeURIComponent(c.name)}`}>
                    {c.name}
                  </Link>
                </Table.Cell>
                <Table.Cell className="font-mono text-xs">{c.fields.map((f) => `${f.name}(${f.dimension}, ${f.metric})`).join(", ") || "none"}</Table.Cell>
                <Table.Cell>{c.items}</Table.Cell>
                <Table.Cell className="text-muted-foreground">{c.description}</Table.Cell>
                <Table.Cell className="text-right">
                  <ConfirmButton label={`Delete ${c.name}`} onConfirm={() => void remove(c.name)} />
                </Table.Cell>
              </Table.Row>
            ))}
          </Table.Body>
        </Table>
      )}
    </div>
  );
}

function CreateForm({ onDone }: { onDone: () => void }) {
  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  const [field, setField] = useState("embedding");
  const [dimension, setDimension] = useState("3");
  const [metric, setMetric] = useState<Metric>("cosine");
  const [quantization, setQuantization] = useState<Quantization>("f32");
  const [error, setError] = useState<unknown>();
  const [busy, setBusy] = useState(false);

  async function submit(e: React.FormEvent) {
    e.preventDefault();
    setBusy(true);
    setError(undefined);
    try {
      await createVectorCollection({ name, description, fields: field ? [{ name: field, dimension: Number(dimension), metric, quantization }] : [] });
      onDone();
    } catch (err) {
      setError(err);
    } finally {
      setBusy(false);
    }
  }

  return (
    <Panel title="New vector collection">
      <form aria-label="New vector collection" onSubmit={(e) => void submit(e)} className="grid gap-3 md:grid-cols-3">
        <label className="text-sm">Name<Input size="sm" className="mt-1 w-full" value={name} onChange={(e) => setName(e.target.value)} required /></label>
        <label className="text-sm md:col-span-2">Description<Input size="sm" className="mt-1 w-full" value={description} onChange={(e) => setDescription(e.target.value)} /></label>
        <label className="text-sm">Field name<Input size="sm" className="mt-1 w-full" value={field} onChange={(e) => setField(e.target.value)} /></label>
        <label className="text-sm">Dimension<Input size="sm" type="number" min={1} max={4096} className="mt-1 w-full" value={dimension} onChange={(e) => setDimension(e.target.value)} /></label>
        <label className="text-sm">Metric
          <select className={`${inputCls} mt-1 w-full`} value={metric} onChange={(e) => setMetric(e.target.value as Metric)}>
            <option value="cosine">cosine</option><option value="l2">l2</option><option value="dot">dot</option>
          </select>
        </label>
        <label className="text-sm">Quantization
          <select className={`${inputCls} mt-1 w-full`} value={quantization} onChange={(e) => setQuantization(e.target.value as Quantization)}>
            <option value="f32">f32</option><option value="f16">f16</option><option value="i8">i8</option>
          </select>
        </label>
        <div className="flex items-end"><Button tone="primary" type="submit" disabled={busy}>Create collection</Button></div>
      </form>
      <div className="mt-3"><ErrorView error={error} /></div>
    </Panel>
  );
}
