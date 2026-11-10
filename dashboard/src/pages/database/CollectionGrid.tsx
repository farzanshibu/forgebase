import { useState } from "react";
import { Link } from "react-router-dom";
import { deleteRecord, listRecords, patchRecord } from "../../api";
import type { CollectionDef, FieldDef, RecordObject } from "../../api";
import { useLoad } from "../../hooks";
import { Button, ConfirmButton, ErrorView, Loading } from "../../components/ui";
import { display, parseCell } from "./values";
import { Table } from "panelui-web/table";
import { Input } from "panelui-web/input";
import { Sel } from "../../components/controls";

const PER_PAGE = 25;

export default function CollectionGrid({ collection }: { collection: CollectionDef }) {
  const name = collection.name;
  const [filterText, setFilterText] = useState("");
  const [filter, setFilter] = useState("");
  const [sort, setSort] = useState("");
  const [page, setPage] = useState(1);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [actionError, setActionError] = useState<unknown>();
  const [edit, setEdit] = useState<{ id: string; field: string; text: string } | null>(null);
  const [bulkField, setBulkField] = useState("");
  const [bulkValue, setBulkValue] = useState("");

  const fields = collection.fields.filter((f) => !f.hidden && f.name !== "id");
  const key = JSON.stringify([name, filter, sort, page]);
  const { data, error, loading, reload } = useLoad((signal) => listRecords(name, { filter, sort, page, per_page: PER_PAGE }, signal), key);

  const toggleSort = (field: string) => {
    setPage(1);
    setSort((s) => (s === field ? `-${field}` : s === `-${field}` ? "" : field));
  };
  const sortMark = (field: string) => (sort === field ? " ▲" : sort === `-${field}` ? " ▼" : "");

  async function run(fn: () => Promise<unknown>) {
    setActionError(undefined);
    try {
      await fn();
      reload();
    } catch (e) {
      setActionError(e);
    }
  }

  async function saveCell(rec: RecordObject) {
    if (!edit) return;
    const field = fields.find((f) => f.name === edit.field);
    let value: unknown;
    try {
      value = parseCell(field, edit.text);
    } catch (e) {
      setActionError(new Error(`${edit.field}: ${(e as Error).message}`));
      return;
    }
    setEdit(null);
    await run(() => patchRecord(name, String(rec.id), { [edit.field]: value }, rec._version));
  }

  async function bulkApply() {
    const field = fields.find((f) => f.name === bulkField);
    if (!field) return;
    let value: unknown;
    try {
      value = parseCell(field, bulkValue);
    } catch (e) {
      setActionError(new Error(`${field.name}: ${(e as Error).message}`));
      return;
    }
    await run(async () => {
      for (const id of selected) await patchRecord(name, id, { [field.name]: value });
    });
  }

  const items = data?.items ?? [];
  const allSelected = items.length > 0 && items.every((r) => selected.has(String(r.id)));
  const totalPages = data?.total_pages ?? undefined;

  return (
    <div className="space-y-3">
      <form
        className="flex flex-wrap items-center gap-2"
        onSubmit={(e) => {
          e.preventDefault();
          setPage(1);
          setFilter(filterText);
        }}
      >
        <Input size="sm"
          aria-label="Filter"
          className="w-72 font-mono"
          placeholder='filter, e.g. price > 10 && name ~ "pen"'
          value={filterText}
          onChange={(e) => setFilterText(e.target.value)}
        />
        <Button type="submit">Apply filter</Button>
        <Link to={`/database/c/${encodeURIComponent(name)}/r/new`} className="ml-auto rounded-md border border-primary bg-primary px-3 py-1.5 text-sm text-primary-foreground hover:bg-primary/90">
          Add record
        </Link>
      </form>

      <ErrorView error={error} />
      <ErrorView error={actionError} />
      {selected.size > 0 && (
        <div className="flex flex-wrap items-center gap-2 rounded-md border border-border p-2 text-sm" aria-label="Bulk actions">
          <span>{selected.size} selected</span>
          <Sel aria-label="Bulk field" value={bulkField} onChange={setBulkField} options={[{ value: "", label: "Set field…" }, ...fields.filter((f) => !f.system).map((f) => ({ value: f.name, label: f.name }))]} />
          <Input size="sm" aria-label="Bulk value" value={bulkValue} onChange={(e) => setBulkValue(e.target.value)} placeholder="value" />
          <Button disabled={!bulkField} onClick={() => void bulkApply()}>Apply to selected</Button>
          <ConfirmButton
            label="Delete selected"
            onConfirm={() =>
              void run(async () => {
                for (const id of selected) await deleteRecord(name, id);
                setSelected(new Set());
              })
            }
          />
        </div>
      )}

      {loading && !data && !error && <Loading what="Loading records" />}
      {data && (
        <div className="overflow-x-auto rounded-lg border border-border">
          <Table className="w-full text-left text-sm">
            <Table.Header className="bg-card text-xs uppercase tracking-wide text-muted-foreground">
              <Table.Row>
                <Table.Head className="w-8 p-2">
                  <input
                    type="checkbox"
                    aria-label="Select all"
                    checked={allSelected}
                    onChange={() => setSelected(allSelected ? new Set() : new Set(items.map((r) => String(r.id))))}
                  />
                </Table.Head>
                <Table.Head className="p-2">
                  <button type="button" onClick={() => toggleSort("id")}>id{sortMark("id")}</button>
                </Table.Head>
                {fields.map((f) => (
                  <Table.Head key={f.name} className="p-2">
                    <button type="button" onClick={() => toggleSort(f.name)}>{f.name}{sortMark(f.name)}</button>
                    <span className="ml-1 normal-case text-muted-foreground">{f.type}</span>
                  </Table.Head>
                ))}
                <Table.Head className="p-2" />
              </Table.Row>
            </Table.Header>
            <Table.Body>
              {items.length === 0 && (
                <Table.Row>
                  <Table.Cell colSpan={fields.length + 3} className="p-4 text-muted-foreground">No records{filter ? " match this filter" : ""}.</Table.Cell>
                </Table.Row>
              )}
              {items.map((rec) => {
                const id = String(rec.id);
                return (
                  <Table.Row key={id} className="border-t border-border">
                    <Table.Cell className="p-2">
                      <input
                        type="checkbox"
                        aria-label={`Select ${id}`}
                        checked={selected.has(id)}
                        onChange={() =>
                          setSelected((s) => {
                            const n = new Set(s);
                            if (n.has(id)) n.delete(id);
                            else n.add(id);
                            return n;
                          })
                        }
                      />
                    </Table.Cell>
                    <Table.Cell className="p-2 font-mono text-xs text-muted-foreground">{id}</Table.Cell>
                    {fields.map((f) => (
                      <Cell key={f.name} field={f} rec={rec} edit={edit?.id === id && edit.field === f.name ? edit : null} setEdit={setEdit} onSave={() => void saveCell(rec)} />
                    ))}
                    <Table.Cell className="space-x-2 whitespace-nowrap p-2 text-right">
                      <Link className="text-primary hover:underline" to={`/database/c/${encodeURIComponent(name)}/r/${encodeURIComponent(id)}`}>JSON</Link>
                      <ConfirmButton label="Delete" onConfirm={() => void run(() => deleteRecord(name, id))} />
                    </Table.Cell>
                  </Table.Row>
                );
              })}
            </Table.Body>
          </Table>
        </div>
      )}

      {data && (
        <div className="flex items-center gap-3 text-sm text-muted-foreground">
          <Button disabled={page <= 1} onClick={() => setPage((p) => p - 1)}>Previous</Button>
          <span>
            Page {page}
            {totalPages ? ` of ${totalPages}` : ""}
            {data.total_items != null ? ` · ${data.total_items} records` : ""}
          </span>
          <Button disabled={totalPages ? page >= totalPages : items.length < PER_PAGE} onClick={() => setPage((p) => p + 1)}>Next</Button>
        </div>
      )}
      <p className="text-xs text-muted-foreground">Double-click a cell to edit; Enter saves, Escape cancels.</p>
    </div>
  );
}

function Cell({ field, rec, edit, setEdit, onSave }: {
  field: FieldDef;
  rec: RecordObject;
  edit: { id: string; field: string; text: string } | null;
  setEdit: (e: { id: string; field: string; text: string } | null) => void;
  onSave: () => void;
}) {
  const value = rec[field.name];
  if (edit) {
    return (
      <Table.Cell className="p-1">
        <Input size="sm"
          autoFocus
          aria-label={`Edit ${field.name}`}
          className="w-full"
          value={edit.text}
          onChange={(e) => setEdit({ ...edit, text: e.target.value })}
          onKeyDown={(e) => {
            if (e.key === "Enter") onSave();
            if (e.key === "Escape") setEdit(null);
          }}
        />
      </Table.Cell>
    );
  }
  return (
    <Table.Cell
      className="max-w-xs truncate p-2 font-mono text-xs"
      title={display(value)}
      onDoubleClick={() => !field.system && setEdit({ id: String(rec.id), field: field.name, text: display(value) })}
    >
      {display(value)}
    </Table.Cell>
  );
}
