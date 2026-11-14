import { useState } from "react";
import { Link, useParams } from "react-router-dom";
import { getStatement } from "../../api";
import { Facts } from "../../components/Facts";
import { Sel } from "../../components/controls";
import { SkeletonLines } from "../../components/Skeleton";
import { Badge } from "../../components/StatusBadge";
import { Button, CopyButton, ErrorView } from "../../components/ui";
import { useLoad } from "../../hooks";
import { Code, Section } from "../jobs/ops";
import Chart from "./Chart";
import { RANGES, fmtInt, fmtMs, fmtRate, rangeOf } from "./util";

/** One statement shape: its totals, percentiles and a series over the range. */
export default function StatementDetail() {
  const { fingerprint = "" } = useParams();
  const [range, setRange] = useState("24h");
  const d = useLoad((s) => getStatement(fingerprint, rangeOf(range), s), `statement:${fingerprint}:${range}`);
  const s = d.data;
  const labels = s?.series.map((p) => new Date(p.at).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })) ?? [];
  return (
    <div className="space-y-4">
      <p><Link to="/insights" className="text-sm font-medium text-primary underline-offset-2 hover:underline">← All statements</Link></p>
      <Section
        title="Statement"
        description={<>Shape <code className="font-mono text-xs">{fingerprint}</code></>}
        actions={<span className="flex gap-2"><Sel aria-label="Time range" value={range} onChange={setRange} options={RANGES.map((r) => ({ value: r.id, label: r.label }))} /><Button onClick={d.reload}>Refresh</Button></span>}
      >
        <ErrorView error={d.error} onRetry={d.reload} />
        {d.loading && !s && <SkeletonLines lines={4} label="Loading statement" />}
        {s && (
          <div className="space-y-4">
            <div className="flex items-start gap-2">
              <Code label="Statement text" className="flex-1 whitespace-pre-wrap">{s.statement}</Code>
              <CopyButton text={s.statement} label="Copy statement" iconOnly />
            </div>
            <Facts
              label="Statement facts"
              columns={4}
              items={[
                ["Area", <Badge key="a">{s.area}</Badge>],
                ["Backend", s.backend],
                ["Calls", fmtInt(s.calls)],
                ["Errors", `${fmtInt(s.errors)} (${fmtRate(s.error_rate)})`],
                ["Rows", fmtInt(s.rows)],
                ["Writes", fmtInt(s.writes)],
                ["Slow runs", fmtInt(s.slow)],
                ["Total time", fmtMs(s.total_ms)],
                ["Fastest", fmtMs(s.min_ms)],
                ["Mean", fmtMs(s.mean_ms)],
                ["Slowest", fmtMs(s.max_ms)],
                ["p50 · p95 · p99", `${fmtMs(s.p50_ms)} · ${fmtMs(s.p95_ms)} · ${fmtMs(s.p99_ms)}`],
              ]}
            />
            <Chart label="Latency" labels={labels} format={fmtMs} series={[{ name: "p95", values: s.series.map((p) => p.p95_ms) }, { name: "mean", values: s.series.map((p) => p.mean_ms) }]} />
            <Chart label="Calls and errors" labels={labels} format={(n) => fmtInt(Math.round(n))} series={[{ name: "calls", values: s.series.map((p) => p.calls) }, { name: "errors", values: s.series.map((p) => p.errors), className: "text-destructive" }]} />
          </div>
        )}
      </Section>
    </div>
  );
}
