import { useState } from "react";
import { ApiError, explainSql, sqlTables } from "../../api";
import type { QueryPlan } from "../../api";
import { Check } from "../../components/controls";
import { EmptyState } from "../../components/EmptyState";
import { runSqlStatement, type SqlRunResult } from "./sqlRequest";
import { expandStar, secretRefusal } from "./sqlSecrets";
import RunAsPicker from "./RunAsPicker";
import { ADMIN, rememberTenant, toRunAs } from "./pgHelpers";
import { getDatabaseInfo } from "./info";
import { useLoad } from "../../hooks";
import { Button, ErrorView, PlanView, Panel } from "../../components/ui";
import { display } from "./values";
import { Table } from "panelui-web/table";
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
  const info = useLoad(getDatabaseInfo, "database-info");
  const postgres = info.data?.backend === "postgres";
  const [history, setHistory] = useState(loadHistory);
  const tables = useLoad(sqlTables, "sql-tables");

  async function go(kind: "run" | "explain", text = sql, secrets = showSecrets) {
    setBusy(true);
    setProblem(undefined);
    try {
      if (kind === "run") {
        setPlan(undefined);
        const as = postgres ? toRunAs(runAs, tenant) : undefined;
        setResult(await runSqlStatement(text, { showSecrets: secrets, runAs: as, readFromReplica: replica }));
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

  return (
    <div className="grid gap-6 lg:grid-cols-[1fr_16rem]">
      <div className="min-w-0 space-y-3">
        {postgres && <RunAsPicker principal={runAs} tenant={tenant} onChange={(p, t) => { setRunAs(p); setTenant(t); }} />}
        {ranAs && (
          <p role="status" className="rounded-md border border-border p-2 text-xs text-muted-foreground">
            Running as <strong className="text-foreground">{ranAs.principal}</strong>{ranAs.tenant ? ` in tenant ${ranAs.tenant}` : ""}: row and field rules apply and table names read the <code>api</code> views. Reads and writes only: schema changes are refused. Each run is recorded in the audit log (<code>sql.run_as</code>).
          </p>
        )}
        <Textarea size="sm" autoResize={false}
          aria-label="SQL"
          spellCheck={false}
          className="h-40 font-mono"
          value={sql}
          onChange={(e) => setSql(e.target.value)}
          onKeyDown={(e) => {
            if ((e.metaKey || e.ctrlKey) && e.key === "Enter") void go("run");
          }}
        />
        <div className="flex gap-2">
          <Button tone="primary" disabled={busy || !sql.trim()} onClick={() => void go("run")}>Run</Button>
          <Button disabled={busy || !sql.trim()} onClick={() => void go("explain")}>Explain</Button>
          <Check className="ml-2" checked={showSecrets} onChange={setShowSecrets} label="Show secrets" description="Reveal masked secret columns. Each use is audited." />
          {postgres && <Check className="ml-2" checked={replica} onChange={setReplica} label="Read from replica" description="Send a read to a replica that has your own writes. Ignored without replicas, and for writes." />}
        </div>
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
        {plan && <Panel title="Query plan"><PlanView plan={plan} /></Panel>}
        {result && (
          <Panel title={`${result.kind} · ${result.rows.length} rows · ${result.duration_ms.toFixed(1)} ms`}>
            {result.rows_affected > 0 && <p className="mb-2 text-sm">{result.rows_affected} rows affected.</p>}
            {result.secrets_masked && <p role="status" className="mb-2 text-sm text-warning">Secret values are masked. Tick “Show secrets” and run again to see them; each use is recorded in the audit log.</p>}
            {result.truncated && <p className="mb-2 text-sm text-warning">Result truncated by the server row limit.</p>}
            {result.columns.length > 0 && (
              <div className="overflow-x-auto">
                <Table className="w-full text-left text-xs" aria-label="Result">
                  <Table.Header className="text-muted-foreground"><Table.Row>{result.columns.map((c) => <Table.Head key={c} className="p-1">{c}</Table.Head>)}</Table.Row></Table.Header>
                  <Table.Body>
                    {result.rows.map((r, i) => (
                      <Table.Row key={i} className="border-t border-border font-mono">
                        {r.map((v, j) => <Table.Cell key={j} className="max-w-xs truncate p-1">{display(v)}</Table.Cell>)}
                      </Table.Row>
                    ))}
                  </Table.Body>
                </Table>
              </div>
            )}
          </Panel>
        )}
      </div>
      <aside className="space-y-4 text-sm">
        <Panel title="Tables">
          <ErrorView error={tables.error} />
          <ul className="space-y-1 font-mono text-xs">
            {tables.data?.map((t) => (
              <li key={t.name}><button type="button" className="text-left hover:text-primary" onClick={() => setSql(`SELECT * FROM "${t.name}" LIMIT 50`)}>{t.name}</button></li>
            ))}
          </ul>
        </Panel>
        <Panel title="History">
          {history.length === 0 && <EmptyState compact title="No queries run yet" why="Statements you run appear here, newest first, so you can run them again." />}
          <ul className="space-y-1 font-mono text-xs">
            {history.map((h) => (
              <li key={h} className="truncate"><button type="button" className="hover:text-primary" title={h} onClick={() => setSql(h)}>{h}</button></li>
            ))}
          </ul>
        </Panel>
      </aside>
    </div>
  );
}
