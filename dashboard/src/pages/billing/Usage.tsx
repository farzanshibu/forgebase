import { useState } from "react";
import { EmptyState } from "../../components/EmptyState";
import { listUsageAlerts, listUsageMeters } from "../../api";
import type { ServerMeterState, UsageMeter } from "../../api";
import { Button } from "../../components/ui";
import { Pager } from "../../components/Pager";
import { SkeletonCards } from "../../components/Skeleton";
import { useLoad } from "../../hooks";
import { formatCount, percent } from "./money";
import { BillingError, Filter, SubjectLink, TableCard } from "./shared";
import { Table } from "panelui-web/table";
import { Input } from "panelui-web/input";
import { Badge as PanelBadge } from "panelui-web/badge";
import { Stat, StatGrid } from "../../components/Stats";
import { Th } from "../../components/TableCard";
import { When } from "../../components/Time";

const PAGE = 50;
const BAR: Record<ServerMeterState, string> = { ok: "bg-success", warn: "bg-warning", critical: "bg-warning", over: "bg-destructive" };
const LABEL: Record<ServerMeterState, string> = { ok: "within quota", warn: "70% used", critical: "90% used", over: "quota reached" };
const TONE = { ok: "success", warn: "warning", critical: "warning", over: "destructive" } as const;
/** `data-state` values kept from the first version of this page. */
const DATA_STATE: Record<ServerMeterState, string> = { ok: "ok", warn: "warn70", critical: "warn90", over: "full" };

export function Meter({ u }: { u: UsageMeter }) {
  const pct = u.limit === null ? 0 : percent(u.used, u.limit);
  return (
    <li className="rounded-xl border border-border bg-card p-4" data-state={DATA_STATE[u.state]}>
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0">
          <p className="truncate font-medium">{u.feature}</p>
          <p className="mt-0.5 text-xs text-muted-foreground"><SubjectLink subject={u.subject} /></p>
        </div>
        <PanelBadge variant={TONE[u.state]} size="sm" dot className="shrink-0">{LABEL[u.state]}</PanelBadge>
      </div>
      <div className="mt-3 flex items-baseline justify-between gap-3">
        <span aria-label={`${u.feature} usage`} className="text-lg font-semibold tabular-nums">{formatCount(u.used)} / {u.limit === null ? "unlimited" : formatCount(u.limit)}</span>
        {u.limit !== null && <span className="text-xs text-muted-foreground tabular-nums">{pct}%</span>}
      </div>
      {u.limit !== null && (
        <div role="progressbar" aria-label={`${u.feature} quota`} aria-valuenow={pct} aria-valuemin={0} aria-valuemax={100} className="mt-2 h-1.5 overflow-hidden rounded-full bg-muted">
          <div className={`h-full rounded-full ${BAR[u.state]}`} style={{ width: `${pct}%` }} />
        </div>
      )}
      <p className="mt-2 text-xs text-muted-foreground">
        Window <When iso={u.window_start} day /> to {u.window_end ? <When iso={u.window_end} day /> : "open"}
        {u.reserved > 0 ? ` · ${formatCount(u.reserved)} reserved` : ""}{u.carried_in > 0 ? ` · ${formatCount(u.carried_in)} carried in` : ""}
      </p>
    </li>
  );
}

export default function Usage() {
  const [subjectIn, setSubjectIn] = useState("");
  const [featureIn, setFeatureIn] = useState("");
  const [minIn, setMinIn] = useState("");
  const [f, setF] = useState({ subject: "", feature: "", min: "" });
  const [offset, setOffset] = useState(0);
  const meters = useLoad(
    (s) => listUsageMeters({ subject: f.subject || undefined, feature: f.feature || undefined, min_percent: f.min ? Number(f.min) : undefined, limit: PAGE, offset }, s),
    `meters:${f.subject}:${f.feature}:${f.min}:${offset}`,
  );
  const alerts = useLoad((s) => (f.subject ? listUsageAlerts(f.subject, s) : Promise.resolve(undefined)), `alerts:${f.subject}`);
  const items = meters.data?.items ?? [];
  const count = (st: ServerMeterState) => items.filter((u) => u.state === st).length;
  const filtered = !!(f.subject || f.feature || f.min);
  return (
    <div className="space-y-4">
      <form className="flex flex-wrap items-center gap-x-4 gap-y-2 rounded-xl border border-border bg-muted/30 px-4 py-2.5" onSubmit={(e) => { e.preventDefault(); setOffset(0); setF({ subject: subjectIn.trim(), feature: featureIn.trim(), min: minIn.trim() }); }}>
        <Filter label="Subject">{(id) => <Input id={id} size="sm" className="w-48 font-mono" value={subjectIn} onChange={(e) => setSubjectIn(e.target.value)} placeholder="All subjects" />}</Filter>
        <Filter label="Feature">{(id) => <Input id={id} size="sm" className="w-36 font-mono" value={featureIn} onChange={(e) => setFeatureIn(e.target.value)} placeholder="All features" />}</Filter>
        <Filter label="Min % used">{(id) => <Input id={id} size="sm" className="w-20 tabular-nums" inputMode="numeric" value={minIn} onChange={(e) => setMinIn(e.target.value)} placeholder="0" />}</Filter>
        <Button type="submit">Show usage</Button>
      </form>
      <BillingError error={meters.error} />
      {meters.loading && !meters.error && <SkeletonCards count={4} label="Loading usage" />}
      {meters.data && (
        <StatGrid label="Usage summary" columns={4}>
          <Stat label="Meters" value={formatCount(meters.data.total)} hint={filtered ? "matching the filters" : "across all customers"} />
          <Stat label="Quota reached" value={formatCount(count("over"))} tone={count("over") ? "bad" : undefined} hint="on this page" />
          <Stat label="Above 90%" value={formatCount(count("critical"))} hint="on this page" />
          <Stat label="Above 70%" value={formatCount(count("warn"))} hint="on this page" />
        </StatGrid>
      )}
      {meters.data && items.length === 0 && (
        filtered
          ? <EmptyState compact title="No usage meters match" why="Clear or change the filters to see more." />
          : <EmptyState title="No usage yet" why="A meter appears once a customer uses a metered feature, such as API calls or seats. Limits come from their plan in Entitlements." />
      )}
      {items.length > 0 && <ul aria-label="Usage meters" className="grid gap-3 md:grid-cols-2 xl:grid-cols-3">{items.map((u) => <Meter key={`${u.subject}:${u.feature}:${u.scope_key}`} u={u} />)}</ul>}
      {meters.data && meters.data.total > PAGE && (
        <Pager page={Math.floor(offset / PAGE) + 1} perPage={PAGE} total={meters.data.total} onPage={(p) => setOffset((p - 1) * PAGE)} loading={meters.loading} label="Usage paging" />
      )}
      {alerts.data && alerts.data.length > 0 && (
        <TableCard title="Quota alerts" count={alerts.data.length} description={`Thresholds ${f.subject} crossed in its current windows.`}>
          <Table framed aria-label="Quota alerts">
            <Table.Header><Table.Row><Th>When</Th><Th>Feature</Th><Th align="end">Threshold</Th><Th align="end">Used</Th></Table.Row></Table.Header>
            <Table.Body>
              {alerts.data.map((a) => (
                <Table.Row key={a.id}>
                  <Table.Cell className="text-muted-foreground"><When iso={a.at} /></Table.Cell>
                  <Table.Cell className="font-mono text-xs">{a.feature}</Table.Cell>
                  <Table.Cell align="end">{a.threshold}%</Table.Cell>
                  <Table.Cell align="end">{formatCount(a.used)} / {formatCount(a.limit)}</Table.Cell>
                </Table.Row>
              ))}
            </Table.Body>
          </Table>
        </TableCard>
      )}
    </div>
  );
}
