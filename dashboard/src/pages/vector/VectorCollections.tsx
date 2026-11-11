import { useState } from "react";
import { Link } from "react-router-dom";
import { Input } from "panelui-web/input";
import { Table } from "panelui-web/table";
import { deleteVectorCollection, listCollections, listVectorCollections, vectorStats } from "../../api";
import type { Metric, Quantization } from "../../api";
import { Sel } from "../../components/controls";
import { ConfirmDialog } from "../../components/ConfirmDialog";
import { CreateSheet } from "../../components/CreateSheet";
import { EmptyState } from "../../components/EmptyState";
import { fieldErrors } from "../../components/ErrorPanel";
import { FormField } from "../../components/FormField";
import { PageHeader } from "../../components/PageHeader";
import { SkeletonTable } from "../../components/Skeleton";
import { Button, ErrorView } from "../../components/ui";
import { useCreateParam, useLoad } from "../../hooks";
import { notify } from "../../toaster";
import { validate } from "../../validate";
import { buildBody, createCollectionBody, loadSample, SAMPLE_NAME } from "./create";
import { bytes } from "./util";

export default function VectorCollections() {
  const list = useLoad(listVectorCollections, "vector-collections");
  const stats = useLoad(vectorStats, "vector-stats");
  const [creating, setCreating] = useCreateParam();
  const [sampleError, setSampleError] = useState<unknown>();
  const [busy, setBusy] = useState(false);
  const reload = () => { list.reload(); stats.reload(); };

  async function sample() {
    setBusy(true);
    setSampleError(undefined);
    try {
      await loadSample();
      notify.created(`Collection ${SAMPLE_NAME}`);
      reload();
    } catch (e) {
      setSampleError(e);
    } finally {
      setBusy(false);
    }
  }

  const newButton = <Button tone="primary" onClick={() => setCreating(true)}>New collection</Button>;
  return (
    <div className="space-y-4">
      <PageHeader studio="vector" primary={newButton} />
      {stats.data && (
        <p aria-label="Vector totals" className="text-sm text-muted-foreground">
          {stats.data.collections} collections · {stats.data.indexes} indexes ({stats.data.building} building) · {stats.data.indexed_vectors} indexed vectors · {bytes(stats.data.memory_bytes)} index memory
        </p>
      )}
      <ErrorView error={sampleError ?? list.error} onRetry={list.reload} />
      {list.loading && !list.error && <SkeletonTable rows={3} columns={5} label="Loading collections" />}
      {list.data && list.data.length === 0 && (
        <EmptyState
          title="No vector collections yet"
          why="Vector collections store embeddings, lists of numbers that stand for meaning, so you can search by similarity. Create one for your own embeddings, or load a small sample to try search."
          action={newButton}
          sample={<Button disabled={busy} onClick={() => void sample()}>{busy ? "Loading sample…" : "Load sample"}</Button>}
          docs="docs/adr/0012-vector.md"
        />
      )}
      {list.data && list.data.length > 0 && (
        <Table aria-label="Vector collections" className="w-full text-left text-sm">
          <Table.Header className="text-xs uppercase tracking-wide text-muted-foreground">
            <Table.Row>
              <Table.Head className="py-2">Name</Table.Head>
              <Table.Head>Fields</Table.Head>
              <Table.Head>Items</Table.Head>
              <Table.Head>Description</Table.Head>
              <Table.Head><span className="sr-only">Actions</span></Table.Head>
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
                  <ConfirmDialog
                    trigger={<Button tone="danger" aria-label={`Delete ${c.name}`}>Delete</Button>}
                    title={`Delete vector collection ${c.name}?`}
                    consequence="Every record and its index are deleted. Apps that search it get 404. This cannot be undone."
                    confirmLabel="Delete collection"
                    confirmText={c.name}
                    onConfirm={async () => { await deleteVectorCollection(c.name); notify.deleted(`Collection ${c.name}`); reload(); }}
                  />
                </Table.Cell>
              </Table.Row>
            ))}
          </Table.Body>
        </Table>
      )}
      <NewCollectionSheet open={creating} onOpenChange={setCreating} onCreated={reload} />
    </div>
  );
}

function NewCollectionSheet({ open, onOpenChange, onCreated }: { open: boolean; onOpenChange: (o: boolean) => void; onCreated: () => void }) {
  const [name, setName] = useState("");
  const [touched, setTouched] = useState(false);
  const [description, setDescription] = useState("");
  const [field, setField] = useState("embedding");
  const [dimension, setDimension] = useState("3");
  const [metric, setMetric] = useState<Metric>("cosine");
  const [quantization, setQuantization] = useState<Quantization>("f32");
  const [source, setSource] = useState("");
  const [saveError, setSaveError] = useState<unknown>();
  const dbCollections = useLoad((s) => listCollections(s).catch(() => []), "collections");
  const body = buildBody({ name, description, field, dimension, metric, quantization, source });
  const server = fieldErrors(saveError);
  const nameError = (touched ? validate.collectionName(name) : undefined) ?? server.name;
  const dimError = validate.integer(1, 4096)(dimension) ?? (dimension.trim() ? undefined : "Enter the number of dimensions.");
  return (
    <CreateSheet
      open={open}
      onOpenChange={onOpenChange}
      title="New vector collection"
      description="A collection holds items; each item has one vector per field, plus optional metadata and text."
      submitLabel="Create collection"
      canSubmit={!validate.collectionName(name) && !dimError}
      onError={setSaveError}
      onSubmit={async () => {
        await createCollectionBody(body);
        onCreated();
        setName(""); setTouched(false); setSaveError(undefined);
        return { what: `Collection ${body.name}`, href: `/vector/c/${encodeURIComponent(body.name)}` };
      }}
    >
      <FormField label="Name" required error={nameError} help="Letters, digits and underscores, starting with a letter.">
        {(p) => <Input {...p} size="sm" value={name} onChange={(e) => { setName(e.target.value); setTouched(true); setSaveError(undefined); }} />}
      </FormField>
      <FormField label="Description">{(p) => <Input {...p} size="sm" value={description} onChange={(e) => setDescription(e.target.value)} />}</FormField>
      <div className="grid grid-cols-2 gap-3">
        <FormField label="Vector field name" help="One name per kind of embedding.">{(p) => <Input {...p} size="sm" value={field} onChange={(e) => setField(e.target.value)} />}</FormField>
        <FormField label="Dimensions" term="dimension" required error={dimError} help="Your embedding model's size, such as 3 or 1536.">{(p) => <Input {...p} size="sm" inputMode="numeric" value={dimension} onChange={(e) => setDimension(e.target.value)} />}</FormField>
        <FormField label="Metric" term="metric" help="Cosine suits text embeddings.">{(p) => <Sel {...p} aria-label="Metric" value={metric} onChange={(v) => setMetric(v as Metric)} options={[{ value: "cosine", label: "cosine" }, { value: "l2", label: "l2" }, { value: "dot", label: "dot" }]} />}</FormField>
        <FormField label="Quantization" help="f32 is exact; f16 and i8 save memory.">{(p) => <Sel {...p} aria-label="Quantization" value={quantization} onChange={(v) => setQuantization(v as Quantization)} options={[{ value: "f32", label: "f32" }, { value: "f16", label: "f16" }, { value: "i8", label: "i8" }]} />}</FormField>
      </div>
      <FormField label="Source collection" help="Optional. Link items to a database collection: every hit is re-checked against that collection's View rule and field mask.">
        {(p) => <Sel {...p} aria-label="Source collection" value={source} onChange={setSource} placeholder="None" options={[{ value: "", label: "None" }, ...(dbCollections.data ?? []).filter((c) => !c.system).map((c) => ({ value: c.name, label: c.name }))]} />}
      </FormField>
      <details className="rounded-md border border-border p-2 text-xs">
        <summary className="cursor-pointer font-medium">What this sends</summary>
        <p className="mt-2 text-muted-foreground"><code className="font-mono">POST /api/v1/vector/collections</code>, the same body your code sends:</p>
        <pre aria-label="Request body" className="mt-1 overflow-auto rounded bg-muted/60 p-2 font-mono">{JSON.stringify(body, null, 2)}</pre>
      </details>
    </CreateSheet>
  );
}
