import { useState } from "react";
import { Outlet, Route, Routes } from "react-router-dom";
import { flushInsights, insightsStatus } from "../../api";
import type { FlushReport } from "../../api";
import { PlatformOnly } from "../../auth/scope";
import { PageHeader } from "../../components/PageHeader";
import { RouteTabs } from "../../components/controls";
import { Button, ErrorView } from "../../components/ui";
import { useLoad } from "../../hooks";
import { Callout } from "../jobs/ops";
import { fmtInt } from "./util";
import Audit from "./Audit";
import Functions from "./Functions";
import FunctionDetail from "./FunctionDetail";
import Regressions from "./Regressions";
import SlowLog from "./SlowLog";
import StatementDetail from "./StatementDetail";
import Statements from "./Statements";
import Usage from "./Usage";

const TABS = [
  { to: "/insights", label: "Statements", end: true },
  { to: "/insights/regressions", label: "Regressions" },
  { to: "/insights/slow", label: "Slow log" },
  { to: "/insights/usage", label: "Usage and cost" },
  { to: "/insights/functions", label: "Functions" },
  { to: "/insights/audit", label: "Audit" },
];

function Home() {
  const status = useLoad(insightsStatus, "insights-status");
  const [flushing, setFlushing] = useState(false);
  const [flushed, setFlushed] = useState<FlushReport>();
  const [error, setError] = useState<unknown>();
  const s = status.data;

  async function flush() {
    setFlushing(true);
    setError(undefined);
    try {
      setFlushed(await flushInsights());
      status.reload();
    } catch (e) {
      setError(e);
    } finally {
      setFlushing(false);
    }
  }

  return (
    <div>
      <PageHeader studio="insights" secondary={<Button disabled={flushing} onClick={() => void flush()}>{flushing ? "Writing…" : "Flush statistics now"}</Button>}>
        {s && (
          <p className="mt-1 text-xs text-muted-foreground">
            Slow means over {fmtInt(s.slow_query_ms)} ms · written every {s.flush_interval_secs} s · {fmtInt(s.fingerprints_in_memory)} statement shapes in memory · kept {s.retention_days} days
          </p>
        )}
      </PageHeader>
      <RouteTabs label="Insights sections" tabs={TABS} className="mt-4" />
      <div className="mt-6 space-y-4">
        <ErrorView error={error} />
        {flushed && <Callout tone="ok" role="status">Wrote {flushed.statements} statement shape{flushed.statements === 1 ? "" : "s"}, {flushed.slow_statements} slow statement{flushed.slow_statements === 1 ? "" : "s"} and {flushed.functions} function profile{flushed.functions === 1 ? "" : "s"}{flushed.slow_dropped > 0 ? `; ${flushed.slow_dropped} slow statements were dropped to stay within the limit` : ""}.</Callout>}
        {s && !s.enabled && <Callout tone="warn" role="note" title="Statement statistics are off">Turn on <code>observability.query_stats</code> in Features and settings to collect them. Usage, functions and audit still work.</Callout>}
        <Outlet />
      </div>
    </div>
  );
}

/** Operations insights (ADR-0036): statements, usage and estimated cost, function profiles and audit investigation. */
export default function InsightsPage() {
  return (
    <PlatformOnly what="Insights">
      <Routes>
        <Route element={<Home />}>
          <Route index element={<Statements />} />
          <Route path="queries/:fingerprint" element={<StatementDetail />} />
          <Route path="regressions" element={<Regressions />} />
          <Route path="slow" element={<SlowLog />} />
          <Route path="usage" element={<Usage />} />
          <Route path="functions" element={<Functions />} />
          <Route path="functions/:name" element={<FunctionDetail />} />
          <Route path="audit" element={<Audit />} />
        </Route>
        <Route path="*" element={<p className="text-muted-foreground">No such page in Insights.</p>} />
      </Routes>
    </PlatformOnly>
  );
}
