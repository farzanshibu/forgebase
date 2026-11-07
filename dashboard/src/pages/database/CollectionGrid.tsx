import { useState } from "react";
import { Link } from "react-router-dom";
import { deleteRecord, listRecords, patchRecord } from "../../api";
import type { CollectionDef, FieldDef, RecordObject } from "../../api";
import { useLoad } from "../../hooks";
import { Button, ConfirmButton, ErrorView, Loading, inputCls } from "../../components/ui";
import { display, parseCell } from "./values";

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
        <input
          aria-label="Filter"
          className={`${inputCls} w-72 font-mono`}
          placeholder='filter, e.g. price > 10 && name ~ "pen"'
          value={filterText}
          onChange={(e) => setFilterText(e.target.value)}
        />
        <Button type="submit">Apply filter</Button>
        <Link to={`/database/c/${encodeURIComponent(name)}/r/new`} className="ml-auto rounded-md border border-amber-500 bg-amber-500 px-3 py-1.5 text-sm text-zinc-950 hover:bg-amber-400">
          Add record
        </Link>
      </form>

      <ErrorView error={error} />
      <ErrorView error={actionError} />
      {selected.size > 0 && (
        <div className="flex flex-wrap items-center gap-2 rounded-md border border-zinc-800 p-2 text-sm" aria-label="Bulk actions">
          <span>{selected.size} selected</span>
          <select aria-label="Bulk field" className={inputCls} value={bulkField} onChange={(e) => setBulkField(e.target.value)}>
            <option value="">Set field…</option>
            {fields.filter((f) => !f.system).map((f) => (
              <option key={f.name}>{f.name}</option>
            ))}
          </select>
          <input aria-label="Bulk value" className={inputCls} value={bulkValue} onChange={(e) => setBulkValue(e.target.value)} placeholder="value" />
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
        <div className="overflow-x-auto rounded-lg border border-zinc-800">
          <table className="w-full text-left text-sm">
            <thead className="bg-zinc-900 text-xs uppercase tracking-wide text-zinc-400">
              <tr>
                <th className="w-8 p-2">
                  <input
                    type="checkbox"
                    aria-label="Select all"
                    checked={allSelected}
                    onChange={() => setSelected(allSelected ? new Set() : new Set(items.map((r) => String(r.id))))}
                  />
                </th>
                <th className="p-2">
                  <button type="button" onClick={() => toggleSort("id")}>id{sortMark("id")}</button>
                </th>
                {fields.map((f) => (
                  <th key={f.name} className="p-2">
                    <button type="button" onClick={() => toggleSort(f.name)}>{f.name}{sortMark(f.name)}</button>
                    <span className="ml-1 normal-case text-zinc-600">{f.type}</span>
                  </th>
                ))}
                <th className="p-2" />
              </tr>
            </thead>
            <tbody>
              {items.length === 0 && (
                <tr>
                  <td colSpan={fields.length + 3} className="p-4 text-zinc-400">No records{filter ? " match this filter" : ""}.</td>
                </tr>
              )}
              {items.map((rec) => {
                const id = String(rec.id);
                return (
                  <tr key={id} className="border-t border-zinc-800">
                    <td className="p-2">
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
                    </td>
                    <td className="p-2 font-mono text-xs text-zinc-400">{id}</td>
                    {fields.map((f) => (
                      <Cell key={f.name} field={f} rec={rec} edit={edit?.id === id && edit.field === f.name ? edit : null} setEdit={setEdit} onSave={() => void saveCell(rec)} />
                    ))}
                    <td className="space-x-2 whitespace-nowrap p-2 text-right">
                      <Link className="text-amber-300 hover:underline" to={`/database/c/${encodeURIComponent(name)}/r/${encodeURIComponent(id)}`}>JSON</Link>
                      <ConfirmButton label="Delete" onConfirm={() => void run(() => deleteRecord(name, id))} />
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>
      )}

      {data && (
        <div className="flex items-center gap-3 text-sm text-zinc-400">
          <Button disabled={page <= 1} onClick={() => setPage((p) => p - 1)}>Previous</Button>
          <span>
            Page {page}
            {totalPages ? ` of ${totalPages}` : ""}
            {data.total_items != null ? ` · ${data.total_items} records` : ""}
          </span>
          <Button disabled={totalPages ? page >= totalPages : items.length < PER_PAGE} onClick={() => setPage((p) => p + 1)}>Next</Button>
        </div>
      )}
      <p className="text-xs text-zinc-500">Double-click a cell to edit; Enter saves, Escape cancels.</p>
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
      <td className="p-1">
        <input
          autoFocus
          aria-label={`Edit ${field.name}`}
          className={`${inputCls} w-full`}
          value={edit.text}
          onChange={(e) => setEdit({ ...edit, text: e.target.value })}
          onKeyDown={(e) => {
            if (e.key === "Enter") onSave();
            if (e.key === "Escape") setEdit(null);
          }}
        />
      </td>
    );
  }
  return (
    <td
      className="max-w-xs truncate p-2 font-mono text-xs"
      title={display(value)}
      onDoubleClick={() => !field.system && setEdit({ id: String(rec.id), field: field.name, text: display(value) })}
    >
      {display(value)}
    </td>
  );
}
