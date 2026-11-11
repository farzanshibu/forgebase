import { useState } from "react";
import { Input } from "panelui-web/input";
import { Table } from "panelui-web/table";
import { deleteVectorItem, getVectorItem, upsertVectors, vectorSearch } from "../../api";
import type { VectorCollection, VectorItem } from "../../api";
import { Sel } from "../../components/controls";
import { ConfirmDialog } from "../../components/ConfirmDialog";
import { CreateSheet, DetailSheet } from "../../components/CreateSheet";
import { EmptyState } from "../../components/EmptyState";
import { fieldErrors } from "../../components/ErrorPanel";
import { FormField } from "../../components/FormField";
import { SkeletonLines, SkeletonTable } from "../../components/Skeleton";
import { Button, ErrorView } from "../../components/ui";
import { useLoad } from "../../hooks";
import { notify } from "../../toaster";
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
  const [adding, setAdding] = useState(false);
  const [fail, setFail] = useState<unknown>();
  const done = () => { reload(); onChanged(); };

  async function remove(id: string) {
    await deleteVectorItem(collection.name, id);
    notify.deleted(`Record ${id}`);
    if (selected === id) setSelected(undefined);
    done();
  }

  if (!field) return <EmptyState title="This collection has no vector field" why="A vector field holds the embedding of each record. Create the collection again with a field to store records." />;
  const addButton = <Button tone="primary" onClick={() => setAdding(true)}>Add record</Button>;
  return (
    <div className="space-y-4">
      <ErrorView error={fail ?? error} onRetry={reload} />
      {loading && !error && <SkeletonTable rows={4} columns={4} label="Loading records" />}
      {data && data.length === 0 && (
        <EmptyState title="No records yet" why="A record is an embedding with optional metadata and text. Add one here, or upsert many from your code." action={addButton} docs="docs/adr/0012-vector.md" />
      )}
      {data && data.length > 0 && (
        <div>
          <div className="mb-2 flex items-center justify-between gap-2">
            <p className="text-sm text-muted-foreground">
              {data.length} records{data.length >= BROWSE_LIMIT ? ` (first ${BROWSE_LIMIT} shown)` : ""}
            </p>
            {addButton}
          </div>
          <Table aria-label="Records" className="w-full text-left text-sm">
            <Table.Header className="text-xs uppercase tracking-wide text-muted-foreground">
              <Table.Row><Table.Head className="py-2">Id</Table.Head><Table.Head>Metadata</Table.Head><Table.Head>Text</Table.Head><Table.Head><span className="sr-only">Actions</span></Table.Head></Table.Row>
            </Table.Header>
            <Table.Body>
              {data.map((h) => (
                <Table.Row key={h.id} className="border-t border-border">
                  <Table.Cell className="py-2"><button type="button" className="font-mono text-primary hover:underline" onClick={() => setSelected(h.id)}>{h.id}</button></Table.Cell>
                  <Table.Cell className="max-w-xs truncate font-mono text-xs text-muted-foreground">{JSON.stringify(h.metadata ?? {})}</Table.Cell>
                  <Table.Cell className="max-w-xs truncate text-muted-foreground">{h.text ?? ""}</Table.Cell>
                  <Table.Cell className="text-right">
                    <ConfirmDialog
                      trigger={<Button tone="danger" aria-label={`Delete ${h.id}`}>Delete</Button>}
                      title={`Delete record ${h.id}?`}
                      consequence="The record and its vectors are removed from the collection and its index."
                      confirmLabel="Delete record"
                      onConfirm={() => remove(h.id).catch((e: unknown) => { setFail(e); })}
                    />
                  </Table.Cell>
                </Table.Row>
              ))}
            </Table.Body>
          </Table>
        </div>
      )}
      {selected && <ItemDetail key={selected} name={collection.name} id={selected} onClose={() => setSelected(undefined)} />}
      {adding && <UpsertSheet collection={collection} onClose={() => setAdding(false)} onDone={done} />}
    </div>
  );
}

function ItemDetail({ name, id, onClose }: { name: string; id: string; onClose: () => void }) {
  const { data, error, loading } = useLoad<VectorItem>((s) => getVectorItem(name, id, s), `vitem/${name}/${id}`);
  return (
    <DetailSheet open onOpenChange={(o) => !o && onClose()} title={`Item ${id}`} description={`In ${name}.`}>
      <ErrorView error={error} />
      {loading && !error && <SkeletonLines lines={4} label="Loading item" />}
      {data && <pre aria-label="Item JSON" className="max-h-[70vh] overflow-auto rounded bg-muted/60 p-3 text-xs text-foreground/80">{JSON.stringify(data, null, 2)}</pre>}
    </DetailSheet>
  );
}

function UpsertSheet({ collection, onClose, onDone }: { collection: VectorCollection; onClose: () => void; onDone: () => void }) {
  const [id, setId] = useState("");
  const [field, setField] = useState(collection.fields[0]?.name ?? "");
  const [vector, setVector] = useState("");
  const [metadata, setMetadata] = useState("");
  const [text, setText] = useState("");
  const [tried, setTried] = useState(false);
  const [saveError, setSaveError] = useState<unknown>();
  const dim = collection.fields.find((f) => f.name === field)?.dimension ?? 3;
  const example = Array.from({ length: Math.min(dim, 4) }, (_, i) => (0.1 * (i + 1)).toFixed(1)).join(", ") + (dim > 4 ? ", …" : "");

  const check = (): { vectorError?: string; metaError?: string } => {
    const out: { vectorError?: string; metaError?: string } = {};
    try { const v = parseVector(vector); if (v.length !== dim) out.vectorError = `This field takes ${dim} numbers; you entered ${v.length}.`; } catch (e) { out.vectorError = (e as Error).message; }
    try { parseJsonObject(metadata, "Metadata"); } catch (e) { out.metaError = (e as Error).message; }
    return out;
  };
  const problems = check();
  const server = fieldErrors(saveError);
  return (
    <CreateSheet
      open
      onOpenChange={(o) => !o && onClose()}
      title="Add record"
      description={`Adds or replaces a record in ${collection.name}.`}
      submitLabel="Save record"
      onError={setSaveError}
      onSubmit={async () => {
        setTried(true);
        if (problems.vectorError || problems.metaError) throw new Error("Some fields need attention.");
        const item = { ...(id ? { id } : {}), vectors: { [field]: parseVector(vector) }, metadata: parseJsonObject(metadata, "Metadata"), ...(text ? { text } : {}) };
        const r = await upsertVectors(collection.name, [item]);
        onDone();
        return { what: `Record ${r.ids.join(", ")}` };
      }}
    >
      <FormField label="Id" help="Leave empty to generate one. An existing id replaces that record." error={server.id}>{(p) => <Input {...p} size="sm" value={id} onChange={(e) => setId(e.target.value)} />}</FormField>
      {collection.fields.length > 1 && (
        <FormField label="Field">{(p) => <Sel {...p} aria-label="Field" value={field} onChange={setField} options={collection.fields.map((f) => ({ value: f.name, label: `${f.name} (${f.dimension})` }))} />}</FormField>
      )}
      <FormField label="Vector" required term="embedding" error={tried ? problems.vectorError : undefined} help={`${dim} numbers, separated by commas or as a JSON array.`}>
        {(p) => <Input {...p} size="sm" className="font-mono" value={vector} onChange={(e) => setVector(e.target.value)} placeholder={example} />}
      </FormField>
      <FormField label="Metadata" error={tried ? problems.metaError : undefined} help="Optional JSON object you can filter searches by.">
        {(p) => <Input {...p} size="sm" className="font-mono" value={metadata} onChange={(e) => setMetadata(e.target.value)} placeholder='{"topic": "cooking"}' />}
      </FormField>
      <FormField label="Text" help="Optional. Used by hybrid search, which mixes similarity with keyword matching.">{(p) => <Input {...p} size="sm" value={text} onChange={(e) => setText(e.target.value)} />}</FormField>
    </CreateSheet>
  );
}
