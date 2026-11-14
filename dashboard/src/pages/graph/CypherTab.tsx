import { useCallback, useEffect, useRef, useState } from "react";
import { Input } from "panelui-web/input";
import { Table } from "panelui-web/table";
import { Textarea } from "panelui-web/textarea";
import { ApiError, cypher, dropCypherGraph, graphStats } from "../../api";
import type { CypherResult, CypherStatus } from "../../api";
import { ConfirmDialog } from "../../components/ConfirmDialog";
import { FormField } from "../../components/FormField";
import { SkeletonLines } from "../../components/Skeleton";
import { Badge } from "../../components/StatusBadge";
import { Button, ErrorView, Panel } from "../../components/ui";
import { queryPosition } from "../../errors";
import { useLoad } from "../../hooks";
import { notify } from "../../toaster";
import { Cell } from "./cypherCells";

export const LIMIT = { min: 1, max: 1000, def: 100 };
export const TIMEOUT = { min: 1, max: 30000, def: 10000 };
/** How often the build is checked while a query waits for the derived graph. */
export const POLL_MS = 1000;

/** What each state of the derived graph means, in plain words. */
export function stateWords(c: CypherStatus): string {
  switch (c.state) {
    case "none": return "Not built yet. It is built from your nodes and edges the first time you run a query.";
    case "materialising": return `Building the graph for openCypher: ${c.done} of ${c.nodes + c.edges} items copied.`;
    case "ready": return "Ready. Your queries run on an up-to-date copy of your graph.";
    case "stale": return "Out of date, for example after your data moved. It is rebuilt automatically the next time you run a query.";
    case "failed": return `The last build failed${c.error ? `: ${c.error}` : ""}. Running a query tries again.`;
  }
}

function StatusStrip({ status, onDropped }: { status: CypherStatus; onDropped: () => void }) {
  const total = status.nodes + status.edges;
  const pct = total > 0 ? Math.min(100, Math.round((status.done / total) * 100)) : 0;
  return (
    <div aria-label="Derived graph status" data-state={status.state} className="mb-4 rounded-lg border border-border p-3 text-sm">
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div>
          <p className="font-medium">Derived graph: {status.state}</p>
          <p role="status" className="text-muted-foreground">{stateWords(status)}</p>
        </div>
        {status.state !== "none" && (
          <ConfirmDialog
            trigger={<Button tone="danger" />}
            triggerLabel="Drop derived graph"
            title="Drop the derived graph?"
            consequence="This deletes only the copy that openCypher reads. Your nodes and edges are not touched, and the copy is rebuilt the next time you run a query (large graphs take a moment)."
            confirmLabel="Drop derived graph"
            tone="warning"
            onConfirm={async () => {
              const r = await dropCypherGraph();
              notify.deleted(r.dropped ? "Derived graph" : "Derived graph (there was none)");
              onDropped();
            }}
          />
        )}
      </div>
      {status.state === "materialising" && (
        <div role="progressbar" aria-label="Building the graph" aria-valuemin={0} aria-valuemax={100} aria-valuenow={pct} className="mt-2 h-2 w-full overflow-hidden rounded bg-muted">
          <div className="h-full bg-primary transition-all" style={{ width: `${pct}%` }} />
        </div>
      )}
    </div>
  );
}

export const EXAMPLES = [
  { name: "People", text: "MATCH (p:Person) RETURN p", note: "Every node labelled Person." },
  { name: "Who knows whom", text: "MATCH (a:Person)-[r:KNOWS]->(b:Person) RETURN a.id AS who, b.id AS knows", note: "One row per KNOWS edge." },
  { name: "Friends of friends", text: "MATCH (a {id: $start})-[:KNOWS*1..2]->(b) RETURN DISTINCT b", note: "Uses the parameter start." , params: '{ "start": "alice" }' },
  { name: "Paths", text: "MATCH p = (a {id: $start})-[:KNOWS*1..3]->(b) RETURN p", note: "Returns whole paths. Path variables need Apache AGE; the built-in engine says where it stops.", params: '{ "start": "alice" }' },
  { name: "Count by label", text: "MATCH (n) RETURN n.label AS label, count(*) AS total", note: "How many nodes per label." },
] as { name: string; text: string; note: string; params?: string }[];

/** Whole number in [min, max] or an error sentence; empty means "use the default". */
function bounded(text: string, b: { min: number; max: number }, what: string): { value?: number; error?: string } {
  if (!text.trim()) return {};
  const n = Number(text);
  if (!Number.isInteger(n) || n < b.min || n > b.max) return { error: `${what} must be a whole number from ${b.min} to ${b.max}.` };
  return { value: n };
}

/** openCypher query editor, results table and the graph's status (FB-163). */
export default function CypherTab() {
  const [query, setQuery] = useState(EXAMPLES[0]!.text);
  const [params, setParams] = useState("{}");
  const [limit, setLimit] = useState(String(LIMIT.def));
  const [timeout, setTimeoutMs] = useState(String(TIMEOUT.def));
  const [touched, setTouched] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>();
  const [result, setResult] = useState<CypherResult>();
  const stats = useLoad(graphStats, "graph-stats-cypher");
  /** Set while a query waits for the build; `live` is the latest status from polling. */
  const [waiting, setWaiting] = useState(false);
  const [live, setLive] = useState<CypherStatus>();
  const runRef = useRef<() => Promise<void>>(async () => {});

  const limitCheck = bounded(limit, LIMIT, "The limit");
  const timeoutCheck = bounded(timeout, TIMEOUT, "The timeout");
  let paramsValue: Record<string, unknown> | undefined;
  let paramsError: string | undefined;
  if (params.trim() && params.trim() !== "{}") {
    try {
      const v: unknown = JSON.parse(params);
      if (typeof v !== "object" || v === null || Array.isArray(v)) paramsError = "Parameters must be a JSON object, like { \"name\": \"alice\" }.";
      else paramsValue = v as Record<string, unknown>;
    } catch (e) {
      paramsError = `That is not valid JSON: ${(e as Error).message}`;
    }
  }
  const queryError = !query.trim() ? "Write a query first." : undefined;
  const invalid = !!(queryError || paramsError || limitCheck.error || timeoutCheck.error);

  const run = useCallback(async () => {
    setTouched(true);
    if (invalid) return;
    setBusy(true);
    setError(undefined);
    setResult(undefined);
    try {
      setResult(await cypher({ query, ...(paramsValue ? { params: paramsValue } : {}), limit: limitCheck.value ?? LIMIT.def, timeout_ms: timeoutCheck.value ?? TIMEOUT.def }));
    } catch (e) {
      if (e instanceof ApiError && e.code === "graph_materialising") {
        setWaiting(true);
        setLive(undefined);
      } else setError(e);
    } finally {
      setBusy(false);
    }
  }, [invalid, query, paramsValue, limitCheck.value, timeoutCheck.value]);

  useEffect(() => {
    runRef.current = run;
  });

  // The server answered 409 graph_materialising: check the build until it is ready, then run the query.
  useEffect(() => {
    if (!waiting) return;
    let off = false;
    const timer = setInterval(() => {
      graphStats().then(
        (s) => {
          if (off) return;
          const c = s.cypher;
          if (!c) return;
          setLive(c);
          if (c.state === "ready" || c.state === "failed") {
            clearInterval(timer);
            setWaiting(false);
            stats.reload();
            if (c.state === "ready") void runRef.current();
            else setError(new Error(c.error ? `Building the graph failed: ${c.error}` : "Building the graph failed."));
          }
        },
        (e: unknown) => {
          if (off) return;
          clearInterval(timer);
          setWaiting(false);
          setError(e);
        },
      );
    }, POLL_MS);
    return () => {
      off = true;
      clearInterval(timer);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [waiting]);

  const status = waiting ? live ?? stats.data?.cypher ?? undefined : stats.data?.cypher ?? undefined;
  if (stats.loading && !stats.data) return <SkeletonLines lines={4} label="Loading openCypher" />;
  if (stats.error && !stats.data) return <ErrorView error={stats.error} onRetry={stats.reload} />;
  // Without a derived-graph status the server runs the built-in engine (ADR-0034): nothing to build, nothing to drop.
  const show = (msg?: string) => (touched ? msg : undefined);
  return (
    <div className="space-y-4">
      <Panel title="openCypher">
        {status && <StatusStrip status={status} onDropped={() => { setResult(undefined); stats.reload(); }} />}
        <p className="mb-3 text-sm text-muted-foreground">Ask questions of your graph with openCypher. Queries only read; nothing you run here changes your data. They run on Apache AGE where PostgreSQL has it, and on ForgeBase&apos;s built-in engine (a read-only subset) everywhere else.</p>
        <div className="mb-3 flex flex-wrap items-center gap-2" role="group" aria-label="Example queries">
          <span className="text-xs text-muted-foreground">Examples:</span>
          {EXAMPLES.map((x) => (
            <Button key={x.name} title={x.note} onClick={() => { setQuery(x.text); setParams(x.params ?? "{}"); setResult(undefined); setError(undefined); }}>{x.name}</Button>
          ))}
        </div>
        <FormField label="Query" error={show(queryError)} help="One read-only query ending in RETURN with named items. RETURN * is not allowed.">
          {(p) => <Textarea {...p} size="sm" autoResize={false} className="h-32 w-full font-mono" value={query} onChange={(e) => setQuery(e.target.value)} />}
        </FormField>
        <div className="mt-3 grid gap-3 sm:grid-cols-[1fr_10rem_10rem]">
          <FormField label="Parameters (JSON)" error={show(paramsError)} help="Used as $name in the query.">
            {(p) => <Textarea {...p} size="sm" autoResize={false} className="h-20 w-full font-mono" value={params} onChange={(e) => setParams(e.target.value)} />}
          </FormField>
          <FormField label="Limit" error={show(limitCheck.error)} help={`${LIMIT.min} to ${LIMIT.max}, default ${LIMIT.def}.`}>
            {(p) => <Input {...p} size="sm" type="number" min={LIMIT.min} max={LIMIT.max} value={limit} onChange={(e) => setLimit(e.target.value)} />}
          </FormField>
          <FormField label="Timeout (ms)" error={show(timeoutCheck.error)} help={`${TIMEOUT.min} to ${TIMEOUT.max}, default ${TIMEOUT.def}.`}>
            {(p) => <Input {...p} size="sm" type="number" min={TIMEOUT.min} max={TIMEOUT.max} value={timeout} onChange={(e) => setTimeoutMs(e.target.value)} />}
          </FormField>
        </div>
        <div className="mt-3"><Button tone="primary" disabled={busy || waiting} onClick={() => void run()}>{waiting ? "Waiting for the graph…" : busy ? "Running…" : "Run query"}</Button></div>
      </Panel>
      <ErrorView error={error} />
      <ErrorPlace error={error} query={query} />
      {result && <Results result={result} limit={limitCheck.value ?? LIMIT.def} />}
    </div>
  );
}

/** The line of the query the server named, with a caret under the column (cypher_invalid, cypher_unsupported, cypher_too_complex). */
function ErrorPlace({ error, query }: { error: unknown; query: string }) {
  const pos = queryPosition(error);
  const line = pos ? query.split("\n")[pos.line - 1] : undefined;
  if (!pos || line === undefined) return null;
  return (
    <figure aria-label="Where the query stops" className="rounded-lg border border-border bg-muted/40 p-3">
      <figcaption className="mb-1 text-xs text-muted-foreground">Line {pos.line}, column {pos.column}</figcaption>
      <pre className="overflow-x-auto font-mono text-xs leading-5"><code>{line}{"\n"}{" ".repeat(Math.max(0, pos.column - 1))}^</code></pre>
    </figure>
  );
}

const ENGINE_WORDS = {
  age: "Answered by Apache AGE on the derived copy of your graph.",
  portable: "Answered by ForgeBase's built-in engine, reading your graph tables directly: a read-only subset of openCypher.",
} as const;

function Results({ result, limit }: { result: CypherResult; limit: number }) {
  const n = result.rows.length;
  return (
    <Panel title={`${n} ${n === 1 ? "row" : "rows"}`} actions={result.engine ? <Badge tone={result.engine === "age" ? "info" : "neutral"} title={ENGINE_WORDS[result.engine]}>engine: {result.engine}</Badge> : undefined}>
      {result.truncated && (
        <p role="note" className="mb-2 text-sm text-warning">Truncated: more rows matched than the limit of {limit}. Raise the limit (up to {LIMIT.max}) or narrow the query.</p>
      )}
      {n === 0 ? (
        <p className="text-sm text-muted-foreground">The query ran and matched nothing.</p>
      ) : (
        <div className="overflow-x-auto rounded-lg border border-border">
          <Table aria-label="Query results" framed density="compact" className="w-full text-left text-sm">
            <Table.Header className="bg-muted/40 text-xs text-muted-foreground">
              <Table.Row>{result.columns.map((c, i) => <Table.Head key={i} className="py-1 pr-4 font-mono">{c}</Table.Head>)}</Table.Row>
            </Table.Header>
            <Table.Body>
              {result.rows.map((r, i) => (
                <Table.Row key={i} className="border-t border-border align-top hover:bg-muted/40">
                  {r.map((v, j) => <Table.Cell key={j} className="py-1 pr-4"><Cell value={v} /></Table.Cell>)}
                </Table.Row>
              ))}
            </Table.Body>
          </Table>
        </div>
      )}
    </Panel>
  );
}
