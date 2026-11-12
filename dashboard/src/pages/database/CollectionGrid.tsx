import { useState, type ReactNode } from "react";
import { useSearchParams } from "react-router-dom";
import { ArrowDown01Icon, ArrowUp01Icon, Delete02Icon, FilterHorizontalIcon, PencilEdit02Icon, Refresh01Icon, Search01Icon } from "@hugeicons/core-free-icons";
import { Input } from "panelui-web/input";
import { Table } from "panelui-web/table";
import { deleteRecord, listRecords, patchRecord } from "../../api";
import type { CollectionDef, FieldDef } from "../../api";
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
import { Icon, IconButton, TableCard } from "./parts";
import { PER_PAGE, display, fuzzyThresholdError, parseCell, readGridState } from "./values";

const FILTER_HELP: [string, string][] = [
  ["=  !=", "equal, not equal: status = \"live\""],
  [">  >=  <  <=", "compare numbers and dates: price > 10"],
  ["~  !~", "contains, does not contain: title ~ \"pen\""],
  ["&&  ||  ( )", "combine tests: price > 10 && title ~ \"pen\""],
  ["@now", "the current time: created > @now"],
];

const NUMERIC = new Set(["number"]);
/** The row actions stay in view while a wide grid scrolls sideways. */
const STICKY_END = "sticky right-0 z-[1] bg-card shadow-[inset_1px_0_0_var(--border)] group-hover:bg-[color-mix(in_oklab,var(--muted)_40%,var(--card))] group-aria-selected:bg-muted";
const STICKY_HEAD = "sticky right-0 z-[1] bg-[color-mix(in_oklab,var(--muted)_40%,var(--card))] shadow-[inset_1px_0_0_var(--border)]";
const STRUCTURED = new Set(["json", "array", "geo", "vector"]);

/** One grid cell: numbers right-aligned, booleans as a coloured word, structured values in quiet mono, empty values as a faint dash. */
function Cell({ field, value }: { field: FieldDef; value: unknown }) {
  const type = String(field.type);
  const text = display(value);
  const end = NUMERIC.has(type);
  let body: ReactNode = text;
  if (value === null || value === undefined || text === "") body = <span className="text-muted-foreground/60" aria-label="empty">–</span>;
  else if (type === "bool") body = <span className={value === true ? "font-medium text-success" : "text-muted-foreground"}>{text}</span>;
  return (
    <Table.Cell
      align={end ? "end" : "start"}
      title={text || undefined}
      className={`max-w-[18rem] truncate ${STRUCTURED.has(type) || typeof value === "object" ? "font-mono text-xs text-muted-foreground" : ""}`}
    >
      {body}
    </Table.Cell>
  );
}

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
  const ariaSort = (field: string) => (sort === field ? "ascending" : sort === `-${field}` ? "descending" : undefined);
  const sortIcon = (field: string) =>
    sort === field ? <Icon icon={ArrowUp01Icon} size={13} className="text-foreground" /> : sort === `-${field}` ? <Icon icon={ArrowDown01Icon} size={13} className="text-foreground" /> : null;

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
  const total = data?.total_items ?? undefined;
  const sortHead = (field: string, label: ReactNode, extra?: ReactNode, end = false) => (
    <Table.Head key={field} align={end ? "end" : "start"} aria-sort={ariaSort(field)}>
      <span className={`inline-flex items-center gap-1.5 ${end ? "flex-row-reverse" : ""}`}>
        <button type="button" className="inline-flex items-center gap-1 rounded-sm font-medium text-foreground/80 hover:text-foreground focus-visible:outline-2 focus-visible:outline-ring" onClick={() => toggleSort(field)}>
          {label}
          {sortIcon(field)}
        </button>
        {extra}
      </span>
    </Table.Head>
  );

  return (
    <div className="space-y-3">
      <ErrorView error={actionError} />
      <TableCard
        label="Records"
        toolbar={<FilterForm key={`${filter}|${search}|${fuzzy}|${threshold}`} filter={filter} search={search} fuzzy={fuzzy} threshold={threshold} trigram={trigram} onApply={(a) => setState({ page: "1", filter: a.filter, search: a.search, fuzzy: a.fuzzy ? "1" : "", ft: a.fuzzy ? a.threshold : "" })} />}
        footer={data && items.length > 0 ? <Pager page={page} perPage={PER_PAGE} total={total} hasNext={items.length >= PER_PAGE} onPage={(p) => setState({ page: String(p) })} loading={loading} label="Records pages" /> : undefined}
      >
        {selected.size > 0 && (
          <div className="flex flex-wrap items-center gap-2 border-b border-border bg-primary/5 px-4 py-2 text-sm" role="group" aria-label="Bulk actions">
            <span className="font-medium tabular-nums">{selected.size} selected</span>
            <span className="mx-1 h-4 w-px bg-border" aria-hidden="true" />
            <Sel aria-label="Bulk field" value={bulkField} onChange={setBulkField} options={[{ value: "", label: "Set field…" }, ...fields.filter((f) => !f.system).map((f) => ({ value: f.name, label: f.name }))]} />
            <Input size="sm" aria-label="Bulk value" className="w-40" value={bulkValue} onChange={(e) => setBulkValue(e.target.value)} placeholder="value" />
            <Button disabled={!bulkField} onClick={() => void bulkApply()}>Apply to selected</Button>
            <span className="flex-1" />
            <Button onClick={() => setSelected(new Set())}>Clear selection</Button>
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
        {error !== undefined && <div className="p-4"><ErrorView error={error} onRetry={reload} /></div>}
        {loading && !data && !error && <div className="p-4"><SkeletonTable rows={5} columns={Math.min(fields.length + 2, 7)} label="Loading records" /></div>}
        {data && items.length === 0 && (
          <div className="p-4">
            {filtered ? (
              <EmptyState compact title="No records match this filter" why="Change the filter or the search, or clear them to see every record." action={<Button onClick={() => setState({ page: "1", filter: "", search: "", fuzzy: "", ft: "" })}>Clear filter</Button>} />
            ) : (
              <EmptyState title="No records yet" why={`Records are the rows of ${name}. Add the first one with the form, or import many from the Import / export tab.`} action={<Button tone="primary" onClick={() => setState({ record: "new" })}>Add record</Button>} docs="docs/database.md" />
            )}
          </div>
        )}
        {data && items.length > 0 && (
          <Table framed density="compact" aria-label={`Records of ${name}`} className={loading ? "opacity-60 transition-opacity" : undefined}>
            <Table.Header className="bg-muted/40">
              <Table.Row>
                <Table.Head className="w-10">
                  <Check aria-label="Select all" checked={allSelected} onChange={() => setSelected(allSelected ? new Set() : new Set(items.map((r) => String(r.id))))} />
                </Table.Head>
                {sortHead("id", "id")}
                {fields.map((f) =>
                  sortHead(
                    f.name,
                    f.name,
                    <span className="rounded bg-muted px-1 py-0.5 font-mono text-[0.625rem] font-normal text-muted-foreground" title={`Field type: ${String(f.type)}`}>{String(f.type)}</span>,
                    NUMERIC.has(String(f.type)),
                  ),
                )}
                <Table.Head align="end" className={STICKY_HEAD}><span className="sr-only">Actions</span></Table.Head>
              </Table.Row>
            </Table.Header>
            <Table.Body>
              {items.map((rec) => {
                const id = String(rec.id);
                const isSelected = selected.has(id);
                return (
                  <Table.Row
                    key={id}
                    aria-selected={isSelected || undefined}
                    className="group cursor-pointer"
                    onClick={(e) => {
                      if ((e.target as HTMLElement).closest("button, input, a, [role=checkbox], [role=dialog]")) return;
                      setState({ record: id });
                    }}
                  >
                    <Table.Cell>
                      <Check aria-label={`Select ${id}`} checked={isSelected} onChange={() => setSelected((s) => { const n = new Set(s); if (n.has(id)) n.delete(id); else n.add(id); return n; })} />
                    </Table.Cell>
                    <Table.Cell className="max-w-[10rem] truncate font-mono text-xs text-muted-foreground" title={id}>{id}</Table.Cell>
                    {fields.map((f) => <Cell key={f.name} field={f} value={rec[f.name]} />)}
                    <Table.Cell align="end" className={`whitespace-nowrap ${STICKY_END}`}>
                      <span className="inline-flex items-center gap-0.5">
                        <IconButton icon={PencilEdit02Icon} className="size-7" label={`Open ${id}`} onClick={() => setState({ record: id })} />
                        <ConfirmDialog
                          trigger={<IconButton icon={Delete02Icon} className="size-7" tone="danger" label={`Delete ${id}`} />}
                          title={`Delete record ${id}?`}
                          consequence="The record is removed. Apps that read it get 404."
                          confirmLabel="Delete record"
                          onConfirm={() => run(() => deleteRecord(name, id).then(() => notify.deleted(`Record ${id}`)))}
                        />
                      </span>
                    </Table.Cell>
                  </Table.Row>
                );
              })}
            </Table.Body>
          </Table>
        )}
      </TableCard>
      {data && items.length > 0 && (
        <div className="flex items-center justify-between gap-2 text-xs text-muted-foreground">
          <span>Click a row to edit it in a form. Click a column name to sort.</span>
          <button type="button" className="inline-flex items-center gap-1 rounded-sm hover:text-foreground focus-visible:outline-2 focus-visible:outline-ring" onClick={reload} disabled={loading}>
            <Icon icon={Refresh01Icon} size={13} className={loading ? "animate-spin motion-reduce:animate-none" : ""} />
            Refresh
          </button>
        </div>
      )}

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
      className="w-full space-y-2"
      onSubmit={(e) => {
        e.preventDefault();
        if (thresholdError) return;
        apply(filterText.trim());
      }}
    >
      <div className="flex flex-wrap items-center gap-2">
        <label className="relative block">
          <span className="pointer-events-none absolute inset-y-0 left-2.5 flex items-center text-muted-foreground"><Icon icon={Search01Icon} size={14} /></span>
          <Input size="sm" aria-label="Search" className="w-52 pl-8" placeholder="Search text" value={searchText} onChange={(e) => setSearchText(e.target.value)} />
        </label>
        <label className="relative block min-w-64 flex-1">
          <span className="pointer-events-none absolute inset-y-0 left-2.5 flex items-center text-muted-foreground"><Icon icon={FilterHorizontalIcon} size={14} /></span>
          <Input size="sm"
            aria-label="Filter"
            className="w-full pl-8 font-mono"
            placeholder='price > 10 && title ~ "pen"'
            value={filterText}
            onChange={(e) => setFilterText(e.target.value)}
          />
        </label>
        <Button type="submit" disabled={!!thresholdError}>Apply filter</Button>
        {(filter || search) && <Button type="button" onClick={() => onApply({ filter: "", search: "", fuzzy: false, threshold: "" })}>Clear filter</Button>}
        <details className="relative text-xs text-muted-foreground">
          <summary className="cursor-pointer select-none rounded-sm px-1 py-1.5 hover:text-foreground">Filter help</summary>
          <div className="absolute right-0 z-20 mt-2 w-[26rem] max-w-[80vw] rounded-lg border border-border bg-popover p-3 text-popover-foreground shadow-lg">
            <p className="mb-2 font-medium text-foreground">Filter syntax</p>
            <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1.5">
              {FILTER_HELP.map(([op, text]) => (
                <div key={op} className="contents">
                  <dt className="whitespace-pre font-mono text-foreground">{op}</dt>
                  <dd className="text-muted-foreground">{text}</dd>
                </div>
              ))}
            </dl>
          </div>
        </details>
      </div>
      {trigram && (
        <div className="flex flex-wrap items-center gap-3">
          <Toggle checked={fuzzyOn} onChange={setFuzzyOn} label="Fuzzy" description="Match near spellings with trigrams. The field needs an index of kind trigram." />
          {fuzzyOn && (
            <span className="inline-flex items-center gap-2 text-xs">
              <Input size="sm" aria-label="Fuzzy threshold" aria-invalid={thresholdError ? true : undefined} className="w-20" inputMode="decimal" placeholder="0.3" value={thresholdText} onChange={(e) => setThresholdText(e.target.value)} />
              {thresholdError && <span role="alert" className="text-destructive">{thresholdError}</span>}
            </span>
          )}
        </div>
      )}
    </form>
  );
}
