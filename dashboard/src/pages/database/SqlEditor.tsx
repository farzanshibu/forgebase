import { useState } from "react";
import { ApiError, explainSql, getTopology, sqlTables } from "../../api";
import type { QueryPlan } from "../../api";
import { Check, Sel } from "../../components/controls";
import { HelpTip } from "../../components/HelpTip";
import { runSqlStatement, type SqlRunResult } from "./sqlRequest";
import { expandStar, secretRefusal } from "./sqlSecrets";
import RunAsPicker from "./RunAsPicker";
import { ADMIN, rememberTenant, toRunAs } from "./pgHelpers";
import { getDatabaseInfo } from "./info";
import { useLoad } from "../../hooks";
import { Button, ErrorView, PlanView } from "../../components/ui";
import { display } from "./values";
import { Table } from "panelui-web/table";
import { Badge } from "panelui-web/badge";
import { PlayIcon, Table01Icon } from "@hugeicons/core-free-icons";
import { Icon, TableCard } from "./parts";
import { Textarea } from "panelui-web/textarea";

const HISTORY_KEY = "forgebase.sql.history";
const MAX_HISTORY = 30;

function loadHistory(): string[] {
  try {
    const v: unknown = JSON.parse(localStorage.getItem(HISTORY_KEY) ?? "[]");
    return Array.isArray(v) ? v.filter((x): x is string => typeof x === "string") : [];
  } catch {
    return [];
  }
}

export default function SqlEditor() {
  const [sql, setSql] = useState("SELECT 1 AS one");
  const [result, setResult] = useState<SqlRunResult>();
  const [plan, setPlan] = useState<QueryPlan>();
  const [problem, setProblem] = useState<unknown>();
  const [busy, setBusy] = useState(false);
  const [showSecrets, setShowSecrets] = useState(false);
  const [runAs, setRunAs] = useState(ADMIN);
  const [tenant, setTenant] = useState("");
  const [replica, setReplica] = useState(false);
  const [runOn, setRunOn] = useState("");
  const info = useLoad(getDatabaseInfo, "database-info");
  const postgres = info.data?.backend === "postgres";
  const [history, setHistory] = useState(loadHistory);
  const tables = useLoad(sqlTables, "sql-tables");
  // "Run on" shows only when the server is sharded (FB-171): a failed or single-server answer hides it.
  const topology = useLoad(async (s) => { if (!postgres) return undefined; try { return await getTopology(s); } catch { return undefined; } }, `sql-topology:${postgres}`);
  const shardSets = postgres && topology.data?.backend === "postgres" && topology.data.sets.some((s) => s.kind === "shard") ? topology.data.sets : [];

  async function go(kind: "run" | "explain", text = sql, secrets = showSecrets) {
    setBusy(true);
    setProblem(undefined);
    try {
      if (kind === "run") {
        setPlan(undefined);
        const as = postgres ? toRunAs(runAs, tenant) : undefined;
        setResult(await runSqlStatement(text, { showSecrets: secrets, runAs: as, readFromReplica: replica, set: shardSets.length ? runOn : undefined }));
        if (as?.tenant) rememberTenant(as.tenant);
        const next = [text, ...history.filter((h) => h !== text)].slice(0, MAX_HISTORY);
        setHistory(next);
        localStorage.setItem(HISTORY_KEY, JSON.stringify(next));
        tables.reload();
      } else {
        setResult(undefined);
        setPlan(await explainSql(text));
      }
    } catch (e) {
      setProblem(e);
    } finally {
      setBusy(false);
    }
  }

  const refusal = secretRefusal(problem);
  const expanded = refusal ? expandStar(sql, tables.data ?? [], refusal.column ? [refusal.column] : []) : undefined;
  const ranAs = postgres ? toRunAs(runAs, tenant) : undefined;
  const ddlRefused = problem instanceof ApiError && problem.code === "run_as_ddl";

  const canRun = !busy && !!sql.trim();
  const plural = (n: number, one: string, many = `${one}s`) => `${n} ${n === 1 ? one : many}`;

  return (
    <div className="grid gap-6 lg:grid-cols-[minmax(0,1fr)_15rem]">
      <div className="min-w-0 space-y-4">
        {(postgres || shardSets.length > 0) && (
          <div className="flex flex-wrap items-center gap-x-6 gap-y-2">
            {postgres && <RunAsPicker principal={runAs} tenant={tenant} onChange={(p, t) => { setRunAs(p); setTenant(t); }} />}
            {shardSets.length > 0 && (
              <div className="flex flex-wrap items-center gap-2 text-sm">
                <span className="inline-flex items-center gap-1 text-muted-foreground">Run on <HelpTip term="topology_set" /></span>
                <Sel aria-label="Run on" className="w-48" value={runOn} onChange={setRunOn} options={[{ value: "", label: "Control (default)" }, ...shardSets.filter((s) => s.id !== "control").map((s) => ({ value: s.id, label: s.name }))]} />
                <span className="text-xs text-muted-foreground">Each shard holds its own tenants' data. Pick the set whose data you want to query.</span>
              </div>
            )}
          </div>
        )}
        {ranAs && (
          <p role="status" className="rounded-lg border border-info/40 bg-info/10 p-3 text-xs text-muted-foreground">
            Running as <strong className="text-foreground">{ranAs.principal}</strong>{ranAs.tenant ? ` in tenant ${ranAs.tenant}` : ""}: row and field rules apply and table names read the <code>api</code> views. Reads and writes only: schema changes are refused. Each run is recorded in the audit log (<code>sql.run_as</code>).
          </p>
        )}
        <section aria-label="Query" className="overflow-hidden rounded-xl border border-border bg-card transition-shadow has-[textarea:focus-visible]:ring-2 has-[textarea:focus-visible]:ring-ring">
          <Textarea size="sm" autoResize={false}
            aria-label="SQL"
            spellCheck={false}
            className="block h-48 w-full resize-y rounded-none border-0 bg-transparent px-4 py-3 font-mono text-sm shadow-none focus-visible:ring-0 dark:bg-transparent"
            value={sql}
            onChange={(e) => setSql(e.target.value)}
            onKeyDown={(e) => {
              if ((e.metaKey || e.ctrlKey) && e.key === "Enter") void go("run");
            }}
          />
          <div className="flex flex-wrap items-center gap-x-4 gap-y-2 border-t border-border bg-muted/30 px-3 py-2">
            <div className="flex items-center gap-2">
              <Button tone="primary" disabled={!canRun} onClick={() => void go("run")}>
                <Icon icon={PlayIcon} size={14} />
                Run
              </Button>
              <Button disabled={!canRun} onClick={() => void go("explain")}>Explain</Button>
              <span className="hidden text-xs text-muted-foreground sm:inline" aria-hidden="true">
                <kbd className="rounded border border-border bg-background px-1 font-mono text-[0.625rem]">Ctrl</kbd> + <kbd className="rounded border border-border bg-background px-1 font-mono text-[0.625rem]">Enter</kbd> runs
              </span>
            </div>
            <div className="ml-auto flex flex-wrap items-start gap-x-5 gap-y-2">
              <Check checked={showSecrets} onChange={setShowSecrets} label="Show secrets" description="Reveal masked secret columns. Each use is audited." />
              {postgres && <Check checked={replica} onChange={setReplica} label="Read from replica" description="Send a read to a replica that has your own writes. Ignored without replicas, and for writes." />}
            </div>
          </div>
        </section>
        {refusal ? (
          <div role="alert" className="space-y-2 rounded-lg border border-warning/50 bg-warning/10 p-3 text-sm">
            <p className="font-medium">This statement reads secret columns</p>
            <p className="text-muted-foreground">{refusal.canShowSecrets ? "Secret columns, such as password hashes and token keys, are hidden unless you ask to see them. Showing them is recorded in the audit log." : "On PostgreSQL the SQL editor has no right to read secret columns, so Show secrets cannot reveal them."}</p>
            <div className="flex flex-wrap gap-2">
              {refusal.canShowSecrets && <Button tone="primary" disabled={busy} onClick={() => { setShowSecrets(true); void go("run", sql, true); }}>Run with Show secrets</Button>}
              {expanded && <Button tone={refusal.canShowSecrets ? "default" : "primary"} disabled={busy} onClick={() => { setSql(expanded); void go("run", expanded); }}>Run without the secret columns</Button>}
            </div>
            {!expanded && <p className="text-xs text-muted-foreground">Name the columns you need instead of <code>*</code>, leaving the secret ones out.</p>}
            <details className="text-xs text-muted-foreground"><summary className="cursor-pointer">Details</summary><p className="mt-1 break-words">{refusal.message}</p></details>
          </div>
        ) : (
          <ErrorView error={problem} />
        )}
        {ddlRefused && <div><Button onClick={() => setRunAs(ADMIN)}>Switch back to Administrator</Button></div>}
        {plan && <TableCard title="Query plan" description="How the database will run it: the indexes it uses and the tables it reads in full."><div className="p-4"><PlanView plan={plan} /></div></TableCard>}
        {result && (
          <TableCard
            title="Results"
            label="Results"
            actions={
              <span className="flex flex-wrap items-center gap-1.5 text-xs text-muted-foreground">
                <Badge variant="secondary" size="sm">{result.kind}</Badge>
                <span className="tabular-nums">{plural(result.rows.length, "row")}</span>
                <span aria-hidden="true">·</span>
                <span className="tabular-nums">{result.duration_ms.toFixed(1)} ms</span>
              </span>
            }
          >
            {(result.rows_affected > 0 || result.secrets_masked || result.truncated) && (
              <div className="space-y-1 border-b border-border px-4 py-2 text-sm">
                {result.rows_affected > 0 && <p>{plural(result.rows_affected, "row")} affected.</p>}
                {result.secrets_masked && <p role="status" className="text-warning">Secret values are masked. Tick “Show secrets” and run again to see them; each use is recorded in the audit log.</p>}
                {result.truncated && <p className="text-warning">Result truncated by the server row limit.</p>}
              </div>
            )}
            {result.columns.length > 0 ? (
              <Table framed density="compact" maxHeight="28rem" aria-label="Result" className="text-xs">
                <Table.Header className="bg-muted/40"><Table.Row>{result.columns.map((c) => <Table.Head key={c} className="font-mono">{c}</Table.Head>)}</Table.Row></Table.Header>
                <Table.Body>
                  {result.rows.map((r, i) => (
                    <Table.Row key={i} className="font-mono">
                      {r.map((v, j) => (
                        <Table.Cell key={j} align={typeof v === "number" ? "end" : "start"} className="max-w-xs truncate" title={display(v)}>
                          {v === null ? <span className="text-muted-foreground/70">NULL</span> : display(v)}
                        </Table.Cell>
                      ))}
                    </Table.Row>
                  ))}
                </Table.Body>
              </Table>
            ) : (
              <p className="px-4 py-3 text-sm text-muted-foreground">The statement ran and returned no columns.</p>
            )}
          </TableCard>
        )}
        {!result && !plan && !problem && (
          <p className="text-xs text-muted-foreground">Write a statement and press Run. Pick a table on the right to start from <code className="font-mono">SELECT *</code>.</p>
        )}
      </div>
      <aside className="space-y-4 text-sm" aria-label="Tables and history">
        <TableCard title="Tables" count={tables.data?.length}>
          <div className="p-2">
            <ErrorView error={tables.error} />
            <ul className="max-h-72 space-y-0.5 overflow-y-auto font-mono text-xs">
              {tables.data?.map((t) => (
                <li key={t.name}>
                  <button type="button" className="flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left hover:bg-muted hover:text-foreground focus-visible:outline-2 focus-visible:outline-ring" title={`Query ${t.name}`} onClick={() => setSql(`SELECT * FROM "${t.name}" LIMIT 50`)}>
                    <Icon icon={Table01Icon} size={14} className="text-muted-foreground" />
                    <span className="truncate">{t.name}</span>
                  </button>
                </li>
              ))}
            </ul>
          </div>
        </TableCard>
        <TableCard title="History" count={history.length || undefined}>
          <div className="p-2">
            {history.length === 0 && <p className="px-2 py-1.5 text-xs text-muted-foreground">Statements you run appear here, newest first, so you can run them again.</p>}
            <ul className="max-h-72 space-y-0.5 overflow-y-auto font-mono text-xs">
              {history.map((h) => (
                <li key={h}>
                  <button type="button" className="block w-full truncate rounded-md px-2 py-1.5 text-left hover:bg-muted focus-visible:outline-2 focus-visible:outline-ring" title={h} onClick={() => setSql(h)}>{h}</button>
                </li>
              ))}
            </ul>
          </div>
        </TableCard>
      </aside>
    </div>
  );
}
