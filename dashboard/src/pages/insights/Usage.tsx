import { useMemo, useState } from "react";
import { Table } from "panelui-web/table";
import { Input } from "panelui-web/input";
import { USAGE_LEVELS, listUsageMetrics, topUsage, usageSeries } from "../../api";
import type { UsageLevel, UsageRow } from "../../api";
import { EmptyState } from "../../components/EmptyState";
import { HelpTip } from "../../components/HelpTip";
import { Sel } from "../../components/controls";
import { SkeletonTable } from "../../components/Skeleton";
import { Badge } from "../../components/StatusBadge";
import { CardBody, HEAD_ROW, NUM, TableCard } from "../../components/TableCard";
import { Button, ErrorView } from "../../components/ui";
import { useLoad } from "../../hooks";
import { Callout, Section } from "../jobs/ops";
import Chart from "./Chart";
import { fmtMetric, today } from "./util";

const LEVEL_WORDS: Record<UsageLevel, string> = { environment: "Environment", project: "Project", org: "Organization", realm: "Realm" };
const COST = "cost";

/** Cost of one row, always worded as an estimate. */
function Cost({ row }: { row: UsageRow }) {
  if (!row.cost) return <span className="text-muted-foreground">—</span>;
  const lines = row.cost.lines.map((l) => `${l.metric}: ${l.quantity.toLocaleString()}${l.amount_minor !== null ? ` → ${(l.amount_minor / 100).toFixed(2)}` : ""}${l.credits ? `, ${l.credits} credits` : ""}`).join("\n");
  return (
    <span title={lines || undefined}>
      <span className="font-medium">{row.cost.amount} {row.cost.currency}</span>
      <span className="ml-1 text-xs text-muted-foreground">estimate</span>
      {row.cost.credits > 0 && <span className="block text-xs text-muted-foreground">{row.cost.credits.toLocaleString()} credits</span>}
    </span>
  );
}

function Series({ scope, name, metric, onClose }: { scope: string; name: string; metric: string; onClose: () => void }) {
  const s = useLoad((sig) => usageSeries(scope, {}, sig), `usage-series:${scope}`);
  const keys = Object.keys(s.data?.metrics ?? {});
  const [picked, setPicked] = useState("");
  const key = keys.includes(picked) ? picked : keys.includes(metric) ? metric : (keys[0] ?? "");
  const points = s.data?.metrics[key] ?? [];
  return (
    <Section title={`Daily values of ${name}`} description="The last 30 days. Counters are the day's total; gauges are the day's last value." actions={<Button onClick={onClose}>Close</Button>}>
      <ErrorView error={s.error} onRetry={s.reload} />
      {s.data && keys.length === 0 && <EmptyState compact title="No daily values" why="The server writes usage every 15 minutes. Nothing was recorded for this environment in the last 30 days." />}
      {keys.length > 0 && (
        <div className="space-y-3">
          <Sel aria-label="Series metric" value={key} onChange={setPicked} options={keys} />
          <Chart label={key} labels={points.map(([d]) => d)} series={[{ name: key, values: points.map(([, v]) => v) }]} format={(n) => Math.round(n).toLocaleString()} />
        </div>
      )}
    </Section>
  );
}

/** Usage per environment, project, organization or realm, top N by a metric or by estimated cost. */
export default function Usage() {
  const [level, setLevel] = useState<UsageLevel>("environment");
  const [period, setPeriod] = useState<"month" | "day">("month");
  const [date, setDate] = useState(today());
  const [metric, setMetric] = useState("functions.invocations");
  const [n, setN] = useState(10);
  const [series, setSeries] = useState<UsageRow>();
  const metrics = useLoad(listUsageMetrics, "usage-metrics");
  const unit = useMemo(() => Object.fromEntries((metrics.data ?? []).map((m) => [m.key, m.unit])), [metrics.data]);
  const dateValue = period === "month" ? date.slice(0, 7) : date;
  const top = useLoad((s) => topUsage({ metric, n, group_by: level, period, date: dateValue }, s), `usage-top:${level}:${period}:${dateValue}:${metric}:${n}`);
  const r = top.data;

  const metricOptions = [{ value: COST, label: "Estimated cost" }, ...(metrics.data ?? []).map((m) => ({ value: m.key, label: m.key }))];
  if (!metricOptions.some((o) => o.value === metric)) metricOptions.push({ value: metric, label: metric });
  const desc = (metrics.data ?? []).find((m) => m.key === metric)?.description;

  return (
    <div className="space-y-4">
      <Callout tone="info" role="note" title="Costs are estimates">
        Each counter of the period is priced with the metered price of the same feature in your billing catalog, as if the period were one billing period. Gauges are not priced. It is not an invoice.
      </Callout>
      <TableCard
        title={`Top ${n} by ${metric === COST ? "estimated cost" : metric}`}
        count={r?.items.length}
        description={desc ?? (metric === COST ? "Environments (or projects, organizations, realms) that would cost the most this period." : undefined)}
        actions={<Button onClick={top.reload}>Refresh</Button>}
        toolbar={
          <div className="flex flex-wrap items-center gap-2" role="group" aria-label="Usage options">
            <Sel aria-label="Scope" value={level} onChange={(v: UsageLevel) => { setLevel(v); setSeries(undefined); }} options={USAGE_LEVELS.map((l) => ({ value: l, label: `By ${LEVEL_WORDS[l].toLowerCase()}` }))} />
            <Sel aria-label="Period" value={period} onChange={setPeriod} options={[{ value: "month", label: "Month" }, { value: "day", label: "Day" }]} />
            <Input size="sm" aria-label={period === "month" ? "Month" : "Day"} type={period === "month" ? "month" : "date"} className="w-40" value={dateValue} onChange={(e) => e.target.value && setDate(e.target.value.length === 7 ? `${e.target.value}-01` : e.target.value)} />
            <Sel aria-label="Metric" value={metric} onChange={setMetric} options={metricOptions} />
            <Sel aria-label="How many" value={n} onChange={setN} options={[5, 10, 25, 50, 100].map((v) => ({ value: v, label: `Top ${v}` }))} />
          </div>
        }
      >
        <CardBody><ErrorView error={top.error} onRetry={top.reload} /></CardBody>
        {r && !r.cost_available && <CardBody><Callout tone="warn" role="note">No cost can be estimated: billing is off, or the catalog has no price in {r.currency ?? "this currency"} for these metrics. Usage numbers are still shown.</Callout></CardBody>}
        {top.loading && !r && <CardBody><SkeletonTable rows={4} columns={5} label="Loading usage" /></CardBody>}
        {r && r.items.length === 0 && <CardBody><EmptyState compact title="No usage in this period" why="Nothing was recorded for this period yet. Usage is rolled up every 15 minutes." /></CardBody>}
        {r && r.items.length > 0 && (
          <Table framed density="compact" aria-label="Usage">
            <Table.Header className={HEAD_ROW}>
              <Table.Row>
                <Table.Head>{LEVEL_WORDS[level]}</Table.Head>
                <Table.Head className={NUM}>{metric === COST ? "Metric" : metric}</Table.Head>
                <Table.Head className={NUM}><span className="inline-flex items-center gap-1">Estimated cost <Badge tone="info">estimate</Badge><HelpTip term="estimate" /></span></Table.Head>
                <Table.Head><span className="sr-only">Actions</span></Table.Head>
              </Table.Row>
            </Table.Header>
            <Table.Body>
              {r.items.map((row) => {
                const v = row.metrics[metric];
                return (
                  <Table.Row key={`${row.level}:${row.id}`}>
                    <Table.Cell className="py-2">
                      <span className="font-medium">{row.name || row.id}</span>
                      {!row.attributed && <Badge tone="neutral" title="Usage events of an organization or principal, not tied to an environment.">not attributed</Badge>}
                      <span className="block text-xs text-muted-foreground">{[row.realm, row.org, row.project].filter(Boolean).join(" / ") || row.level}</span>
                    </Table.Cell>
                    <Table.Cell className={NUM}>{metric === COST ? "—" : v ? <>{fmtMetric(v.value, unit[metric])}{v.peak !== null && v.peak !== v.value && <span className="block text-xs text-muted-foreground">peak {fmtMetric(v.peak, unit[metric])}</span>}</> : "0"}</Table.Cell>
                    <Table.Cell className={NUM}><Cost row={row} /></Table.Cell>
                    <Table.Cell className="text-right">
                      {row.level === "environment" && row.attributed && <Button aria-label={`Daily series of ${row.name || row.id}`} onClick={() => setSeries(row)}>Series</Button>}
                    </Table.Cell>
                  </Table.Row>
                );
              })}
            </Table.Body>
          </Table>
        )}
      </TableCard>
      {series && <Series key={series.id} scope={`env:${series.id}`} name={series.name || series.id} metric={metric} onClose={() => setSeries(undefined)} />}
    </div>
  );
}
