import { useState } from "react";
import { Link } from "react-router-dom";
import { Table } from "panelui-web/table";
import { Input } from "panelui-web/input";
import { listFunctionStats, listInvocations } from "../../api";
import type { FunctionStat, Invocation } from "../../api";
import { EmptyState } from "../../components/EmptyState";
import { HelpTip } from "../../components/HelpTip";
import { Pager } from "../../components/Pager";
import { Check, Sel } from "../../components/controls";
import { SkeletonTable } from "../../components/Skeleton";
import { Badge } from "../../components/StatusBadge";
import { CardBody, HEAD_ROW, NUM, TableCard } from "../../components/TableCard";
import { Button, ErrorView } from "../../components/ui";
import { formatBytes } from "../../format";
import { useCursorList, useLoad } from "../../hooks";
import { When } from "../jobs/ops";
import InvocationSheet from "./InvocationSheet";
import { RANGES, fmtInt, fmtMs, fmtRate, rangeOf } from "./util";

/** Fuel the heaviest run used against the limit, as a bar and a percentage. */
export function FuelBar({ s }: { s: Pick<FunctionStat, "fuel_max" | "fuel_limit" | "fuel_utilisation" | "function"> }) {
  const pct = Math.min(100, Math.round(s.fuel_utilisation * 100));
  return (
    <div title={`${fmtInt(s.fuel_max)} of ${fmtInt(s.fuel_limit)} fuel`}>
      <div role="meter" aria-label={`Fuel use of ${s.function}`} aria-valuemin={0} aria-valuemax={100} aria-valuenow={pct} aria-valuetext={`${pct}% of the fuel limit`} className="h-1.5 w-24 overflow-hidden rounded-full bg-muted">
        <div className={`h-full ${pct >= 90 ? "bg-destructive" : pct >= 60 ? "bg-warning" : "bg-success"}`} style={{ width: `${pct}%` }} />
      </div>
      <span className="text-xs text-muted-foreground">{pct}% of limit</span>
    </div>
  );
}

function Stats() {
  const [range, setRange] = useState("1h");
  const [fn, setFn] = useState("");
  const list = useLoad((s) => listFunctionStats({ ...rangeOf(range), function: fn || undefined }, s), `fn-stats:${range}:${fn}`);
  const items = list.data?.items ?? [];
  return (
    <TableCard
      title="Function profiles"
      count={list.data?.items.length}
      description={<>Per function and version. A <HelpTip term="cold_start">cold start</HelpTip> loaded its code first; <HelpTip term="fuel">fuel</HelpTip> is the work a run did against its limit.</>}
      actions={<Button onClick={list.reload}>Refresh</Button>}
      toolbar={
        <div className="flex flex-wrap items-center gap-2" role="group" aria-label="Filter profiles">
          <Sel aria-label="Time range" value={range} onChange={setRange} options={RANGES.map((r) => ({ value: r.id, label: r.label }))} />
          <Input size="sm" aria-label="Function name" placeholder="Function name" className="w-48" value={fn} onChange={(e) => setFn(e.target.value)} />
        </div>
      }
    >
      <CardBody><ErrorView error={list.error} onRetry={list.reload} /></CardBody>
      {list.loading && !list.data && <CardBody><SkeletonTable rows={3} columns={8} label="Loading function profiles" /></CardBody>}
      {list.data && items.length === 0 && <CardBody><EmptyState compact title="No function runs in this range" why="Profiles are written every flush interval. Invoke a function, then flush statistics, or widen the range. Profiling needs observability.function_profiling." /></CardBody>}
      {items.length > 0 && (
        <Table framed density="compact" aria-label="Function profiles">
          <Table.Header className={HEAD_ROW}>
            <Table.Row><Table.Head>Function</Table.Head><Table.Head className={NUM}>Runs</Table.Head><Table.Head className={NUM}>Errors</Table.Head><Table.Head className={NUM}>Cold starts</Table.Head><Table.Head className={NUM}>p95</Table.Head><Table.Head>Fuel</Table.Head><Table.Head className={NUM}>Memory</Table.Head><Table.Head className={NUM}>Host calls / run</Table.Head></Table.Row>
          </Table.Header>
          <Table.Body>
            {items.map((s) => (
              <Table.Row key={`${s.function}:${s.version}`}>
                <Table.Cell className="py-2"><Link to={`/insights/functions/${encodeURIComponent(s.function)}`} className="font-medium text-primary underline-offset-2 hover:underline">{s.function}</Link> <Badge>v{s.version}</Badge></Table.Cell>
                <Table.Cell className={NUM}>{fmtInt(s.invocations)}</Table.Cell>
                <Table.Cell className={NUM}>{s.errors > 0 ? <span className="text-destructive">{fmtInt(s.errors)} ({fmtRate(s.error_rate)})</span> : 0}{s.timeouts > 0 && <span className="block text-xs text-muted-foreground">{s.timeouts} timeouts</span>}</Table.Cell>
                <Table.Cell className={NUM}>{fmtInt(s.cold_starts)}</Table.Cell>
                <Table.Cell className={NUM}>{fmtMs(s.p95_ms)}</Table.Cell>
                <Table.Cell><FuelBar s={s} /></Table.Cell>
                <Table.Cell className={NUM}>{formatBytes(s.memory_max_bytes)}<span className="block text-xs text-muted-foreground">of {formatBytes(s.memory_limit_bytes)}</span></Table.Cell>
                <Table.Cell className={NUM}>{s.host_calls_per_invocation}</Table.Cell>
              </Table.Row>
            ))}
          </Table.Body>
        </Table>
      )}
    </TableCard>
  );
}

const STATUSES = ["ok", "failed", "trap", "fuel_exhausted", "memory_limit", "timeout"];

function Invocations() {
  const [fn, setFn] = useState("");
  const [status, setStatus] = useState("");
  const [errors, setErrors] = useState(false);
  const [open, setOpen] = useState<Invocation>();
  const list = useCursorList<Invocation>(
    (cursor, signal) => listInvocations({ function: fn || undefined, status: status || undefined, errors: errors || undefined, cursor, limit: 50 }, signal).then((p) => ({ items: p.items, next: p.next_cursor === null ? null : String(p.next_cursor) })),
    `invocations:${fn}:${status}:${errors}`,
  );
  return (
    <>
      <TableCard
        title="Recent invocations"
        count={list.items.length}
        description="Newest first. Open one for the timeline of its host calls."
        actions={<Button onClick={list.reload}>Refresh</Button>}
        toolbar={
          <div className="flex flex-wrap items-center gap-3" role="group" aria-label="Filter invocations">
            <Input size="sm" aria-label="Invocations of function" placeholder="Function name" className="w-48" value={fn} onChange={(e) => setFn(e.target.value)} />
            <Sel aria-label="Outcome" value={status} onChange={setStatus} options={[{ value: "", label: "Any outcome" }, ...STATUSES]} />
            <Check checked={errors} onChange={setErrors} label="Only failures" />
          </div>
        }
        footer={list.items.length > 0 ? <Pager mode="more" shown={list.items.length} hasMore={list.hasMore} loading={list.loadingMore} onMore={list.loadMore} noun="invocations" /> : undefined}
      >
        <CardBody><ErrorView error={list.error} onRetry={list.reload} /></CardBody>
        {list.loading && <CardBody><SkeletonTable rows={3} columns={6} label="Loading invocations" /></CardBody>}
        {!list.loading && !list.error && list.items.length === 0 && <CardBody><EmptyState compact title="No invocations" why="Nothing matches. Functions keep their last runs for as long as the execution log retention allows." /></CardBody>}
        {list.items.length > 0 && (
          <Table framed density="compact" aria-label="Recent invocations">
            <Table.Header className={HEAD_ROW}>
              <Table.Row><Table.Head>When</Table.Head><Table.Head>Function</Table.Head><Table.Head>Outcome</Table.Head><Table.Head className={NUM}>Took</Table.Head><Table.Head className={NUM}>Host calls</Table.Head><Table.Head><span className="sr-only">Actions</span></Table.Head></Table.Row>
            </Table.Header>
            <Table.Body>
              {list.items.map((i) => (
                <Table.Row key={i.id}>
                  <Table.Cell><When iso={i.created_at} /></Table.Cell>
                  <Table.Cell><span className="font-medium">{i.function}</span> <span className="text-xs text-muted-foreground">v{i.version} · {i.trigger}</span>{i.profile?.cold_start && <Badge tone="warn">cold</Badge>}</Table.Cell>
                  <Table.Cell><Badge status={i.status}>{i.status}</Badge></Table.Cell>
                  <Table.Cell className={NUM}>{fmtMs(i.duration_ms)}</Table.Cell>
                  <Table.Cell className={NUM}>{i.profile ? fmtInt(i.profile.host_calls) : "—"}</Table.Cell>
                  <Table.Cell className="text-right"><Button aria-label={`Timeline of invocation ${i.id}`} onClick={() => setOpen(i)}>Timeline</Button></Table.Cell>
                </Table.Row>
              ))}
            </Table.Body>
          </Table>
        )}
      </TableCard>
      {open && <InvocationSheet invocation={open} onClose={() => setOpen(undefined)} />}
    </>
  );
}

/** Function profiles (cold starts, fuel against its limit) and the recent executions with their host-call timelines. */
export default function Functions() {
  return (
    <div className="space-y-4">
      <Stats />
      <Invocations />
    </div>
  );
}
