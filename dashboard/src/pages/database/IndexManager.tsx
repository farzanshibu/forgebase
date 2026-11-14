import { useState } from "react";
import { analyzeIndexes, createIndex, dropIndex, indexRecommendations, indexStats, listIndexes, rebuildIndex } from "../../api";
import type { IndexDefinition, IndexStats } from "../../api";
import { useLoad } from "../../hooks";
import { Check, Sel } from "../../components/controls";
import { ConfirmDialog } from "../../components/ConfirmDialog";
import { EmptyState } from "../../components/EmptyState";
import { FormField } from "../../components/FormField";
import { HelpTip } from "../../components/HelpTip";
import { SkeletonTable } from "../../components/Skeleton";
import { Button, ErrorView } from "../../components/ui";
import { notify } from "../../toaster";
import { formatBytes } from "../../format";
import { Table } from "panelui-web/table";
import { Input } from "panelui-web/input";
import { Badge } from "panelui-web/badge";
import { TableCard } from "./parts";

/** Indexes of one collection, or of every collection when `collection` is not given. */
export default function IndexManager({ collection }: { collection?: string }) {
  const { data, error, loading, reload } = useLoad((s) => listIndexes(collection, s), `indexes/${collection ?? "*"}`);
  const recs = useLoad(indexRecommendations, "recommendations");
  const [problem, setProblem] = useState<unknown>();
  const [stats, setStats] = useState<Record<string, IndexStats>>({});
  const [coll, setColl] = useState(collection ?? "");
  const [keys, setKeys] = useState("");
  const [unique, setUnique] = useState(false);
  const [indexName, setIndexName] = useState("");
  const [kind, setKind] = useState("btree");
  // The trigram kind is offered on every backend (ADR-0034): pg_trgm where it is active, ForgeBase's portable engine elsewhere.
  const isTrigram = kind === "trigram";

  async function act(fn: () => Promise<unknown>) {
    setProblem(undefined);
    try {
      await fn();
      reload();
      recs.reload();
    } catch (e) {
      setProblem(e);
    }
  }

  const create = (def: IndexDefinition) => act(() => createIndex(def));
  const parseKeys = (s: string) =>
    s.split(",").map((k) => k.trim()).filter(Boolean).map((k) => (k.startsWith("-") ? { path: k.slice(1), desc: true } : { path: k }));

  const recList = recs.data?.filter((r) => !collection || r.definition.collection === collection) ?? [];

  return (
    <div className="space-y-6">
      <ErrorView error={problem} />
      <TableCard
        title="Indexes"
        count={data?.length}
        description={collection ? `Indexes on ${collection}. ForgeBase adds the ones it needs for unique fields and relations.` : "Every index, on every collection."}
        actions={<Button title="Refresh the statistics the query planner uses to choose an index" onClick={() => void act(() => analyzeIndexes(collection))}>Refresh statistics</Button>}
      >
        {error !== undefined && <div className="p-4"><ErrorView error={error} /></div>}
        {loading && !data && !error && <div className="p-4"><SkeletonTable rows={3} columns={6} label="Loading indexes" /></div>}
        {data && data.length === 0 && (
          <div className="p-4">
            <EmptyState compact title="No indexes yet" why="An index is a sorted copy of one or more fields that makes filters and sorts on them fast. Create one below, or take a recommendation from the slow-query log." />
          </div>
        )}
        {data && data.length > 0 && (
          <Table framed density="compact" aria-label="Indexes">
            <Table.Header className="bg-muted/40">
              <Table.Row>
                <Table.Head>Name</Table.Head>
                {!collection && <Table.Head>Collection</Table.Head>}
                <Table.Head>Columns</Table.Head>
                <Table.Head>Origin</Table.Head>
                <Table.Head align="end">Rows</Table.Head>
                <Table.Head align="end">Size</Table.Head>
                <Table.Head align="end"><span className="sr-only">Actions</span></Table.Head>
              </Table.Row>
            </Table.Header>
            <Table.Body>
              {data.map((ix) => {
                const st = stats[ix.name] ?? ix.stats;
                return (
                  <Table.Row key={ix.name}>
                    <Table.Cell className="py-2">
                      <span className="flex flex-wrap items-center gap-1.5">
                        <span className="font-mono text-xs">{ix.name}</span>
                        {ix.unique && <Badge variant="warning" size="sm">unique</Badge>}
                        {(ix.kind === "trigram" || ix.definition?.kind === "trigram") && <Badge variant="info" size="sm" title="Speeds up fuzzy search and ~ filters">trigram</Badge>}
                      </span>
                    </Table.Cell>
                    {!collection && <Table.Cell>{ix.collection ?? ix.table}</Table.Cell>}
                    <Table.Cell className="font-mono text-xs text-muted-foreground">{ix.columns.join(", ")}</Table.Cell>
                    <Table.Cell><Badge variant={ix.origin === "manual" || ix.origin === "sql" ? "outline" : "secondary"} size="sm">{ix.origin}</Badge></Table.Cell>
                    <Table.Cell align="end">{st.rows_estimate ?? "–"}</Table.Cell>
                    <Table.Cell align="end">{st.size_bytes != null ? formatBytes(st.size_bytes) : "–"}</Table.Cell>
                    <Table.Cell align="end" className="whitespace-nowrap">
                      <span className="inline-flex items-center gap-1.5">
                        <Button onClick={() => void act(async () => { const st = await indexStats(ix.name); setStats((m) => ({ ...m, [ix.name]: st })); })}>Stats</Button>
                        <Button onClick={() => void act(() => rebuildIndex(ix.name))}>Rebuild</Button>
                        {(ix.origin === "manual" || ix.origin === "sql") && (
                          <ConfirmDialog
                            trigger={<Button tone="danger" aria-label={`Drop ${ix.name}`}>Drop</Button>}
                            title={`Drop index ${ix.name}?`}
                            consequence="Queries that used it get slower. The records are not touched."
                            confirmLabel="Drop index"
                            onConfirm={async () => { await dropIndex(ix.name); notify.deleted(`Index ${ix.name}`); reload(); recs.reload(); }}
                          />
                        )}
                      </span>
                    </Table.Cell>
                  </Table.Row>
                );
              })}
            </Table.Body>
          </Table>
        )}
      </TableCard>

      <div className="grid gap-6 xl:grid-cols-2">
        <section aria-label="Create index" className="rounded-xl border border-border bg-card p-5">
          <h2 className="text-sm font-medium">Create index</h2>
          <p className="mt-1 mb-4 text-xs text-muted-foreground"><HelpTip term="index">An index</HelpTip> speeds up the filters and sorts that use its fields.</p>
          <form
            className="space-y-4 text-sm"
            onSubmit={(e) => {
              e.preventDefault();
              void create({ name: indexName || undefined, collection: coll, keys: parseKeys(keys), unique: unique && !isTrigram, ...(isTrigram ? { kind: "trigram" } : {}) });
            }}
          >
            <div className="grid gap-4 sm:grid-cols-2">
              {!collection && <FormField label="Collection" required>{(p) => <Input {...p} size="sm" className="font-mono" value={coll} onChange={(e) => setColl(e.target.value)} />}</FormField>}
              <FormField label="Keys" required help="Comma separated; put - before a field to sort it descending.">{(p) => <Input {...p} size="sm" className="font-mono" value={keys} onChange={(e) => setKeys(e.target.value)} placeholder="price, -created" />}</FormField>
              <FormField label="Name" help="Optional. ForgeBase names it from the keys.">{(p) => <Input {...p} size="sm" className="font-mono" value={indexName} onChange={(e) => setIndexName(e.target.value)} />}</FormField>
              <FormField label="Kind" help={isTrigram ? "Trigram: makes fuzzy search and contains (~) filters on text fields fast. It runs on every database: on pg_trgm where PostgreSQL has it, on ForgeBase's own engine elsewhere." : "Btree: the usual sorted index."}>
                {(p) => <Sel {...p} value={kind} onChange={setKind} options={[{ value: "btree", label: "btree" }, { value: "trigram", label: "trigram (fuzzy search)" }]} />}
              </FormField>
            </div>
            <div className="flex flex-wrap items-center justify-between gap-3">
              <Check checked={unique && !isTrigram} disabled={isTrigram} onChange={setUnique} label="Unique" description="Reject a second record with the same keys." />
              <Button type="submit" tone="primary" disabled={!keys.trim() || !coll}>Create index</Button>
            </div>
          </form>
        </section>

        <TableCard title="Recommendations" count={recList.length || undefined} description="From the slow-query log: indexes that would have sped up slow statements.">
          <div className="p-4">
            <ErrorView error={recs.error} />
            {recs.data?.length === 0 && <EmptyState compact title="No recommendations" why="When the slow-query log shows a query that an index would speed up, it is suggested here." />}
            {recList.length > 0 && (
              <ul className="divide-y divide-border text-sm">
                {recList.map((r, i) => (
                  <li key={i} className="flex items-center justify-between gap-3 py-2.5 first:pt-0 last:pb-0">
                    <span className="min-w-0">
                      <span className="block truncate font-mono text-xs">{r.definition.collection}({r.definition.keys.map((k) => (k.desc ? `-${k.path}` : k.path)).join(", ")})</span>
                      <span className="mt-0.5 block text-xs text-muted-foreground">{r.reason} · seen {r.occurrences}×, up to {r.max_duration_ms} ms</span>
                    </span>
                    <Button onClick={() => void create(r.definition)}>Create</Button>
                  </li>
                ))}
              </ul>
            )}
          </div>
        </TableCard>
      </div>
    </div>
  );
}
