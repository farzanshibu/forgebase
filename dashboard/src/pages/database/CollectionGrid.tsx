import { useState } from "react";
import { useSearchParams } from "react-router-dom";
import { Input } from "panelui-web/input";
import { Table } from "panelui-web/table";
import { deleteRecord, listRecords, patchRecord } from "../../api";
import type { CollectionDef } from "../../api";
import { Check, Sel, Toggle } from "../../components/controls";
import { ConfirmDialog } from "../../components/ConfirmDialog";
import { EmptyState } from "../../components/EmptyState";
import { Pager } from "../../components/Pager";
import { SkeletonTable } from "../../components/Skeleton";
import { Button, ErrorView } from "../../components/ui";
import { useLoad } from "../../hooks";
import { notify } from "../../toaster";
import RecordEditor from "./RecordEditor";
import { getDatabaseInfo } from "./info";
import { PER_PAGE, display, fuzzyThresholdError, parseCell, readGridState } from "./values";

const FILTER_HELP: [string, string][] = [
  ["=  !=", "equal, not equal: status = \"live\""],
  [">  >=  <  <=", "compare numbers and dates: price > 10"],
  ["~  !~", "contains, does not contain: title ~ \"pen\""],
  ["&&  ||  ( )", "combine tests: price > 10 && title ~ \"pen\""],
  ["@now", "the current time: created > @now"],
];

export default function CollectionGrid({ collection, collections }: { collection: CollectionDef; collections: CollectionDef[] }) {
  const name = collection.name;
  const [params, setParams] = useSearchParams();
  const { filter, sort, page, record, search, fuzzy, threshold } = readGridState(params);
  const trigram = useLoad(getDatabaseInfo, "database-info").data?.features?.trigram?.state === "active";
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [actionError, setActionError] = useState<unknown>();
  const [bulkField, setBulkField] = useState("");
  const [bulkValue, setBulkValue] = useState("");

  /** Change URL state; keeps the other keys and drops the ones set to "". */
  const setState = (next: Partial<Record<"filter" | "sort" | "page" | "record" | "search" | "fuzzy" | "ft", string>>, replace = false) =>
    setParams((p) => {
      const n = new URLSearchParams(p);
      for (const [k, v] of Object.entries(next)) {
        if (v === "" || (k === "page" && v === "1")) n.delete(k);
        else if (v !== undefined) n.set(k, v);
      }
      n.delete("new");
      return n;
    }, { replace });

  const fields = collection.fields.filter((f) => !f.hidden && f.name !== "id");
  const key = JSON.stringify([name, filter, sort, page, search, fuzzy, threshold]);
  const { data, error, loading, reload } = useLoad((signal) => listRecords(name, { filter, sort, page, per_page: PER_PAGE, search: search || undefined, fuzzy: fuzzy && !!search, fuzzy_threshold: threshold.trim() === "" || fuzzyThresholdError(threshold) ? undefined : Number(threshold) }, signal), key);

  const toggleSort = (field: string) => setState({ page: "1", sort: sort === field ? `-${field}` : sort === `-${field}` ? "" : field });
  const sortMark = (field: string) => (sort === field ? " ▲" : sort === `-${field}` ? " ▼" : "");
  const ariaSort = (field: string) => (sort === field ? "ascending" : sort === `-${field}` ? "descending" : undefined);

  async function run(fn: () => Promise<unknown>, done?: string) {
    setActionError(undefined);
    try {
      await fn();
      if (done) notify.saved(done);
      reload();
    } catch (e) {
      setActionError(e);
    }
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
    }, `${selected.size} records`);
  }

  const items = data?.items ?? [];
  const allSelected = items.length > 0 && items.every((r) => selected.has(String(r.id)));
  const filtered = !!filter || !!search;

  return (
    <div className="space-y-3">
      <FilterForm key={`${filter}|${search}|${fuzzy}|${threshold}`} filter={filter} search={search} fuzzy={fuzzy} threshold={threshold} trigram={trigram} onApply={(a) => setState({ page: "1", filter: a.filter, search: a.search, fuzzy: a.fuzzy ? "1" : "", ft: a.fuzzy ? a.threshold : "" })} />

      <ErrorView error={error} onRetry={reload} />
      <ErrorView error={actionError} />
      {selected.size > 0 && (
        <div className="flex flex-wrap items-center gap-2 rounded-md border border-border p-2 text-sm" aria-label="Bulk actions">
          <span>{selected.size} selected</span>
          <Sel aria-label="Bulk field" value={bulkField} onChange={setBulkField} options={[{ value: "", label: "Set field…" }, ...fields.filter((f) => !f.system).map((f) => ({ value: f.name, label: f.name }))]} />
          <Input size="sm" aria-label="Bulk value" value={bulkValue} onChange={(e) => setBulkValue(e.target.value)} placeholder="value" />
          <Button disabled={!bulkField} onClick={() => void bulkApply()}>Apply to selected</Button>
          <ConfirmDialog
            trigger={<Button tone="danger">Delete selected</Button>}
            title={`Delete ${selected.size} records?`}
            consequence="They are removed from the collection. Apps that read them get 404."
            confirmLabel="Delete selected"
            onConfirm={async () => {
              for (const id of selected) await deleteRecord(name, id);
              setSelected(new Set());
              notify.deleted(`${selected.size} records`);
              reload();
            }}
          />
        </div>
      )}

      {loading && !data && !error && <SkeletonTable rows={5} columns={fields.length + 3} label="Loading records" />}
      {data && items.length === 0 && (
        filtered ? (
          <EmptyState compact title="No records match this filter" why="Change the filter or the search, or clear them to see every record." action={<Button onClick={() => setState({ page: "1", filter: "", search: "", fuzzy: "", ft: "" })}>Clear filter</Button>} />
        ) : (
          <EmptyState title="No records yet" why={`Records are the rows of ${name}. Add the first one with the form, or import many from the Import / export tab.`} action={<Button tone="primary" onClick={() => setState({ record: "new" })}>Add record</Button>} docs="docs/database.md" />
        )
      )}
      {data && items.length > 0 && (
        <div className="overflow-x-auto rounded-lg border border-border">
          <Table className="w-full text-left text-sm" aria-label={`Records of ${name}`}>
            <Table.Header className="bg-card text-xs uppercase tracking-wide text-muted-foreground">
              <Table.Row>
                <Table.Head className="w-8 p-2">
                  <Check aria-label="Select all" checked={allSelected} onChange={() => setSelected(allSelected ? new Set() : new Set(items.map((r) => String(r.id))))} />
                </Table.Head>
                <Table.Head className="p-2" aria-sort={ariaSort("id")}>
                  <button type="button" onClick={() => toggleSort("id")}>id{sortMark("id")}</button>
                </Table.Head>
                {fields.map((f) => (
                  <Table.Head key={f.name} className="p-2" aria-sort={ariaSort(f.name)}>
                    <button type="button" onClick={() => toggleSort(f.name)}>{f.name}{sortMark(f.name)}</button>
                    <span className="ml-1.5 rounded bg-muted px-1 py-0.5 text-[0.625rem] font-normal normal-case text-muted-foreground" title={`Field type: ${String(f.type)}`}>{String(f.type)}</span>
                  </Table.Head>
                ))}
                <Table.Head className="p-2"><span className="sr-only">Actions</span></Table.Head>
              </Table.Row>
            </Table.Header>
            <Table.Body>
              {items.map((rec) => {
                const id = String(rec.id);
                return (
                  <Table.Row
                    key={id}
                    className="cursor-pointer border-t border-border hover:bg-muted/40"
                    onClick={(e) => {
                      if ((e.target as HTMLElement).closest("button, input, a, [role=checkbox], [role=dialog]")) return;
                      setState({ record: id });
                    }}
                  >
                    <Table.Cell className="p-2">
                      <Check aria-label={`Select ${id}`} checked={selected.has(id)} onChange={() => setSelected((s) => { const n = new Set(s); if (n.has(id)) n.delete(id); else n.add(id); return n; })} />
                    </Table.Cell>
                    <Table.Cell className="p-2 font-mono text-xs text-muted-foreground">{id}</Table.Cell>
                    {fields.map((f) => (
                      <Table.Cell key={f.name} className="max-w-xs truncate p-2 font-mono text-xs" title={display(rec[f.name])}>{display(rec[f.name])}</Table.Cell>
                    ))}
                    <Table.Cell className="space-x-2 whitespace-nowrap p-2 text-right">
                      <Button onClick={() => setState({ record: id })} aria-label={`Open ${id}`}>Open</Button>
                      <ConfirmDialog
                        trigger={<Button tone="danger" aria-label={`Delete ${id}`}>Delete</Button>}
                        title={`Delete record ${id}?`}
                        consequence="The record is removed. Apps that read it get 404."
                        confirmLabel="Delete record"
                        onConfirm={() => run(() => deleteRecord(name, id).then(() => notify.deleted(`Record ${id}`)))}
                      />
                    </Table.Cell>
                  </Table.Row>
                );
              })}
            </Table.Body>
          </Table>
        </div>
      )}
      {data && items.length > 0 && <Pager page={page} perPage={PER_PAGE} total={data.total_items ?? undefined} hasNext={items.length >= PER_PAGE} onPage={(p) => setState({ page: String(p) })} loading={loading} label="Records pages" />}
      {data && items.length > 0 && <p className="text-xs text-muted-foreground">Select a row to edit it in a form.</p>}

      {record && <RecordEditor key={record} collection={collection} collections={collections} recordId={record} onClose={() => setState({ record: "" })} onSaved={reload} />}
    </div>
  );
}


/** The filter box and its help. Keyed by the applied filter, so a link or "Clear filter" resets what is typed. */
function FilterForm({ filter, search, fuzzy, threshold, trigram, onApply }: { filter: string; search: string; fuzzy: boolean; threshold: string; trigram: boolean; onApply: (a: { filter: string; search: string; fuzzy: boolean; threshold: string }) => void }) {
  const [filterText, setFilterText] = useState(filter);
  const [searchText, setSearchText] = useState(search);
  const [fuzzyOn, setFuzzyOn] = useState(fuzzy);
  const [thresholdText, setThresholdText] = useState(threshold);
  const thresholdError = fuzzyOn ? fuzzyThresholdError(thresholdText) : undefined;
  const apply = (f: string) => onApply({ filter: f, search: searchText.trim(), fuzzy: fuzzyOn && trigram, threshold: thresholdText.trim() });
  return (
      <form
        className="flex flex-wrap items-center gap-2"
        onSubmit={(e) => {
          e.preventDefault();
          if (thresholdError) return;
          apply(filterText.trim());
        }}
      >
        <Input size="sm"
          aria-label="Filter"
          className="w-80 font-mono"
          placeholder='price > 10 && title ~ "pen"'
          value={filterText}
          onChange={(e) => setFilterText(e.target.value)}
        />
        <Input size="sm" aria-label="Search" className="w-56" placeholder="Search text" value={searchText} onChange={(e) => setSearchText(e.target.value)} />
        {trigram && <Toggle checked={fuzzyOn} onChange={setFuzzyOn} label="Fuzzy" description="Match near spellings with trigrams. The field needs an index of kind trigram." />}
        {trigram && fuzzyOn && (
          <span className="inline-flex items-center gap-1 text-xs">
            <Input size="sm" aria-label="Fuzzy threshold" aria-invalid={thresholdError ? true : undefined} className="w-20" inputMode="decimal" placeholder="0.3" value={thresholdText} onChange={(e) => setThresholdText(e.target.value)} />
            {thresholdError && <span role="alert" className="text-destructive">{thresholdError}</span>}
          </span>
        )}
        <Button type="submit" disabled={!!thresholdError}>Apply filter</Button>
        {(filter || search) && <Button type="button" onClick={() => onApply({ filter: "", search: "", fuzzy: false, threshold: "" })}>Clear filter</Button>}
        <details className="text-xs text-muted-foreground">
          <summary className="cursor-pointer select-none">Filter help</summary>
          <dl className="mt-2 grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 rounded-md border border-border p-2">
            {FILTER_HELP.map(([op, text]) => (
              <div key={op} className="contents">
                <dt className="font-mono text-foreground">{op}</dt>
                <dd>{text}</dd>
              </div>
            ))}
          </dl>
        </details>
      </form>
  );
}
