import { useState } from "react";
import { Link, useParams } from "react-router-dom";
import { Table } from "panelui-web/table";
import { getFunctionStats } from "../../api";
import { Facts } from "../../components/Facts";
import { HelpTip } from "../../components/HelpTip";
import { Sel } from "../../components/controls";
import { SkeletonLines } from "../../components/Skeleton";
import { Badge } from "../../components/StatusBadge";
import { HEAD_ROW, NUM } from "../../components/TableCard";
import { Button, ErrorView } from "../../components/ui";
import { formatBytes } from "../../format";
import { useLoad } from "../../hooks";
import { Section } from "../jobs/ops";
import Chart from "./Chart";
import { FuelBar } from "./Functions";
import { RANGES, fmtInt, fmtMs, fmtRate, rangeOf } from "./util";

/** One function: each version's profile with its host-call breakdown, and the series over the range. */
export default function FunctionDetail() {
  const { name = "" } = useParams();
  const [range, setRange] = useState("24h");
  const d = useLoad((s) => getFunctionStats(name, rangeOf(range), s), `fn-detail:${name}:${range}`);
  const labels = d.data?.series.map((p) => new Date(p.at).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })) ?? [];
  return (
    <div className="space-y-4">
      <p><Link to="/insights/functions" className="text-sm font-medium text-primary underline-offset-2 hover:underline">← All functions</Link></p>
      <Section title={name} description="Profile per version over the range." actions={<span className="flex gap-2"><Sel aria-label="Time range" value={range} onChange={setRange} options={RANGES.map((r) => ({ value: r.id, label: r.label }))} /><Button onClick={d.reload}>Refresh</Button></span>}>
        <ErrorView error={d.error} onRetry={d.reload} />
        {d.loading && !d.data && <SkeletonLines lines={4} label="Loading function" />}
        {d.data && (
          <div className="space-y-4">
            <Chart label="Runs and errors" labels={labels} format={(n) => fmtInt(Math.round(n))} series={[{ name: "runs", values: d.data.series.map((p) => p.invocations) }, { name: "errors", values: d.data.series.map((p) => p.errors), className: "text-destructive" }]} />
            <Chart label="Latency and cold starts" labels={labels} format={(n) => fmtMs(n)} series={[{ name: "p95", values: d.data.series.map((p) => p.p95_ms) }, { name: "mean", values: d.data.series.map((p) => p.mean_ms) }]} />
            {d.data.versions.length === 0 && <p className="text-sm text-muted-foreground">This function did not run in the range.</p>}
            {d.data.versions.map((v) => (
              <article key={v.version} aria-label={`Version ${v.version}`} className="space-y-3 rounded-lg border border-border p-3">
                <h3 className="flex items-center gap-2 font-medium">Version {v.version} {v.cold_starts > 0 && <Badge tone="warn">{v.cold_starts} cold start{v.cold_starts === 1 ? "" : "s"}</Badge>}</h3>
                <Facts
                  label={`Version ${v.version} facts`}
                  columns={4}
                  framed={false}
                  items={[
                    ["Runs", fmtInt(v.invocations)],
                    ["Errors", `${fmtInt(v.errors)} (${fmtRate(v.error_rate)})`],
                    ["Timeouts", fmtInt(v.timeouts)],
                    ["Cold starts", fmtInt(v.cold_starts)],
                    ["p50 · p95 · p99", `${fmtMs(v.p50_ms)} · ${fmtMs(v.p95_ms)} · ${fmtMs(v.p99_ms)}`],
                    ["Slowest", fmtMs(v.max_ms)],
                    ["Fuel (mean · max)", `${fmtInt(v.fuel_mean)} · ${fmtInt(v.fuel_max)}`],
                    ["Peak memory", `${formatBytes(v.memory_max_bytes)} of ${formatBytes(v.memory_limit_bytes)}`],
                  ]}
                />
                <div className="flex items-center gap-2 text-sm"><span className="text-muted-foreground">Fuel against its limit of {fmtInt(v.fuel_limit)}</span><FuelBar s={v} /></div>
                <h4 className="flex items-center gap-1 text-sm font-medium">Host calls <HelpTip term="host_call" /><span className="font-normal text-muted-foreground">· {v.host_calls_per_invocation} per run</span></h4>
                {v.host.length === 0 ? <p className="text-sm text-muted-foreground">No host calls recorded.</p> : (
                  <Table framed density="compact" aria-label={`Host calls of version ${v.version}`}>
                    <Table.Header className={HEAD_ROW}><Table.Row><Table.Head>Call</Table.Head><Table.Head className={NUM}>Count</Table.Head><Table.Head className={NUM}>Errors</Table.Head><Table.Head className={NUM}>Mean</Table.Head><Table.Head className={NUM}>Slowest</Table.Head><Table.Head className={NUM}>Share of host time</Table.Head></Table.Row></Table.Header>
                    <Table.Body>
                      {v.host.map((h) => (
                        <Table.Row key={h.op}>
                          <Table.Cell className="font-mono text-xs">{h.op}</Table.Cell>
                          <Table.Cell className={NUM}>{fmtInt(h.count)}</Table.Cell>
                          <Table.Cell className={NUM}>{h.errors > 0 ? <span className="text-destructive">{fmtInt(h.errors)}</span> : 0}</Table.Cell>
                          <Table.Cell className={NUM}>{fmtMs(h.mean_ms)}</Table.Cell>
                          <Table.Cell className={NUM}>{fmtMs(h.max_ms)}</Table.Cell>
                          <Table.Cell className={NUM}>{Math.round(h.share * 100)}%</Table.Cell>
                        </Table.Row>
                      ))}
                    </Table.Body>
                  </Table>
                )}
              </article>
            ))}
          </div>
        )}
      </Section>
    </div>
  );
}
