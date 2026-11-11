import { useState } from "react";
import { analyzeIndexes, createIndex, dropIndex, indexRecommendations, indexStats, listIndexes, rebuildIndex } from "../../api";
import type { IndexDefinition, IndexStats } from "../../api";
import { useLoad } from "../../hooks";
import { Check, Sel } from "../../components/controls";
import { getDatabaseInfo } from "./info";
import { ConfirmDialog } from "../../components/ConfirmDialog";
import { EmptyState } from "../../components/EmptyState";
import { FormField } from "../../components/FormField";
import { HelpTip } from "../../components/HelpTip";
import { SkeletonTable } from "../../components/Skeleton";
import { Button, ErrorView, Panel } from "../../components/ui";
import { notify } from "../../toaster";
import { formatBytes } from "../../format";
import { Table } from "panelui-web/table";
import { Input } from "panelui-web/input";

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
  // A trigram index needs pg_trgm: the kind appears only when the server reports that feature active (FB-166).
  const trigram = useLoad(getDatabaseInfo, "database-info").data?.features?.trigram?.state === "active";
  const isTrigram = trigram && kind === "trigram";

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

  return (
    <div className="space-y-4">
      <ErrorView error={error} />
      <ErrorView error={problem} />
      {loading && !data && !error && <SkeletonTable rows={3} columns={7} label="Loading indexes" />}
      {data && data.length === 0 && (
        <EmptyState compact title="No indexes yet" why="An index is a sorted copy of one or more fields that makes filters and sorts on them fast. Create one below, or take a recommendation from the slow-query log." />
      )}
      {data && data.length > 0 && (
        <Table aria-label="Indexes" className="w-full text-left text-sm">
          <Table.Header className="text-xs uppercase tracking-wide text-muted-foreground">
            <Table.Row><Table.Head className="py-1">Name</Table.Head><Table.Head>Collection</Table.Head><Table.Head>Origin</Table.Head><Table.Head>Columns</Table.Head><Table.Head>Rows</Table.Head><Table.Head>Size</Table.Head><Table.Head><span className="sr-only">Actions</span></Table.Head></Table.Row>
          </Table.Header>
          <Table.Body>
            {data.map((ix) => {
              const st = stats[ix.name] ?? ix.stats;
              return (
                <Table.Row key={ix.name} className="border-t border-border">
                  <Table.Cell className="py-1 font-mono text-xs">{ix.name}{ix.unique && <span className="ml-1 text-warning">unique</span>}{(ix.kind === "trigram" || ix.definition?.kind === "trigram") && <span className="ml-1 rounded border border-border px-1 text-[0.625rem] text-muted-foreground" title="Speeds up fuzzy search and ~ filters">trigram</span>}</Table.Cell>
                  <Table.Cell>{ix.collection ?? ix.table}</Table.Cell>
                  <Table.Cell>{ix.origin}</Table.Cell>
                  <Table.Cell className="font-mono text-xs">{ix.columns.join(", ")}</Table.Cell>
                  <Table.Cell>{st.rows_estimate ?? "–"}</Table.Cell>
                  <Table.Cell>{st.size_bytes != null ? formatBytes(st.size_bytes) : "–"}</Table.Cell>
                  <Table.Cell className="space-x-2 whitespace-nowrap text-right">
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
                  </Table.Cell>
                </Table.Row>
              );
            })}
          </Table.Body>
        </Table>
      )}
      <Button onClick={() => void act(() => analyzeIndexes(collection))}>Analyze (refresh statistics)</Button>

      <Panel title="Create index">
        <p className="mb-3 text-sm text-muted-foreground"><HelpTip term="index">An index</HelpTip> speeds up the filters and sorts that use its fields.</p>
        <form
          className="flex flex-wrap items-end gap-3 text-sm"
          onSubmit={(e) => {
            e.preventDefault();
            void create({ name: indexName || undefined, collection: coll, keys: parseKeys(keys), unique: unique && !isTrigram, ...(isTrigram ? { kind: "trigram" } : {}) });
          }}
        >
          {!collection && <FormField label="Collection" required>{(p) => <Input {...p} size="sm" value={coll} onChange={(e) => setColl(e.target.value)} />}</FormField>}
          <FormField label="Keys" required help="Comma separated; put - before a field to sort it descending.">{(p) => <Input {...p} size="sm" className="w-64" value={keys} onChange={(e) => setKeys(e.target.value)} placeholder="price, -created" />}</FormField>
          <FormField label="Name" help="Optional.">{(p) => <Input {...p} size="sm" value={indexName} onChange={(e) => setIndexName(e.target.value)} />}</FormField>
          {trigram && (
            <FormField label="Kind" help={isTrigram ? "Trigram: makes fuzzy search and contains (~) filters on text fields fast." : "Btree: the usual sorted index."}>
              {(p) => <Sel {...p} value={kind} onChange={setKind} options={[{ value: "btree", label: "btree" }, { value: "trigram", label: "trigram (fuzzy search)" }]} />}
            </FormField>
          )}
          <Check checked={unique && !isTrigram} disabled={isTrigram} onChange={setUnique} label="Unique" description="Reject a second record with the same keys." />
          <Button type="submit" tone="primary" disabled={!keys.trim() || !coll}>Create index</Button>
        </form>
      </Panel>

      <Panel title="Recommendations from the slow-query log">
        <ErrorView error={recs.error} />
        {recs.data?.length === 0 && <EmptyState compact title="No recommendations" why="When the slow-query log shows a query that an index would speed up, it is suggested here." />}
        <ul className="space-y-2 text-sm">
          {recs.data?.filter((r) => !collection || r.definition.collection === collection).map((r, i) => (
            <li key={i} className="flex items-center justify-between gap-3">
              <span>
                <span className="font-mono text-xs">{r.definition.collection}({r.definition.keys.map((k) => (k.desc ? `-${k.path}` : k.path)).join(", ")})</span>
                <span className="ml-2 text-muted-foreground">{r.reason} · seen {r.occurrences}×, up to {r.max_duration_ms} ms</span>
              </span>
              <Button onClick={() => void create(r.definition)}>Create</Button>
            </li>
          ))}
        </ul>
      </Panel>
    </div>
  );
}
