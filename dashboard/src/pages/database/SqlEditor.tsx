import { useState } from "react";
import { explainSql, runSql, sqlTables } from "../../api";
import type { QueryPlan, SqlResult } from "../../api";
import { useLoad } from "../../hooks";
import { Button, ErrorView, PlanView, Panel } from "../../components/ui";
import { display } from "./values";
import { Table } from "panelui-web/table";

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
  const [result, setResult] = useState<SqlResult>();
  const [plan, setPlan] = useState<QueryPlan>();
  const [problem, setProblem] = useState<unknown>();
  const [busy, setBusy] = useState(false);
  const [history, setHistory] = useState(loadHistory);
  const tables = useLoad(sqlTables, "sql-tables");

  async function go(kind: "run" | "explain") {
    setBusy(true);
    setProblem(undefined);
    try {
      if (kind === "run") {
        setPlan(undefined);
        setResult(await runSql(sql));
        const next = [sql, ...history.filter((h) => h !== sql)].slice(0, MAX_HISTORY);
        setHistory(next);
        localStorage.setItem(HISTORY_KEY, JSON.stringify(next));
        tables.reload();
      } else {
        setResult(undefined);
        setPlan(await explainSql(sql));
      }
    } catch (e) {
      setProblem(e);
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="grid gap-6 lg:grid-cols-[1fr_16rem]">
      <div className="min-w-0 space-y-3">
        <textarea
          aria-label="SQL"
          spellCheck={false}
          className="h-40 w-full rounded-md border border-border bg-card p-3 font-mono text-sm"
          value={sql}
          onChange={(e) => setSql(e.target.value)}
          onKeyDown={(e) => {
            if ((e.metaKey || e.ctrlKey) && e.key === "Enter") void go("run");
          }}
        />
        <div className="flex gap-2">
          <Button tone="primary" disabled={busy || !sql.trim()} onClick={() => void go("run")}>Run</Button>
          <Button disabled={busy || !sql.trim()} onClick={() => void go("explain")}>Explain</Button>
        </div>
        <ErrorView error={problem} />
        {plan && <Panel title="Query plan"><PlanView plan={plan} /></Panel>}
        {result && (
          <Panel title={`${result.kind} · ${result.rows.length} rows · ${result.duration_ms.toFixed(1)} ms`}>
            {result.rows_affected > 0 && <p className="mb-2 text-sm">{result.rows_affected} rows affected.</p>}
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
          {history.length === 0 && <p className="text-muted-foreground">Nothing run yet.</p>}
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
