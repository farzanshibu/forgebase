import { useState } from "react";
import { Link } from "react-router-dom";
import { Table } from "panelui-web/table";
import { listRegressions } from "../../api";
import { EmptyState } from "../../components/EmptyState";
import { Sel } from "../../components/controls";
import { SkeletonTable } from "../../components/Skeleton";
import { Badge } from "../../components/StatusBadge";
import { CardBody, HEAD_ROW, NUM, TableCard } from "../../components/TableCard";
import { Button, ErrorView } from "../../components/ui";
import { useLoad } from "../../hooks";
import { When } from "../jobs/ops";
import { fmtInt, fmtMs, fmtRate, oneLine, statementHref } from "./util";

const WINDOWS = [{ value: 3600, label: "Last hour" }, { value: 6 * 3600, label: "Last 6 hours" }];
const BASELINES = [{ value: 86_400, label: "the day before" }, { value: 7 * 86_400, label: "the week before" }];

/** Statements whose recent p95 grew against an earlier baseline, or whose error rate did. */
export default function Regressions() {
  const [window, setWindow] = useState(3600);
  const [baseline, setBaseline] = useState(86_400);
  const list = useLoad((s) => listRegressions({ window, baseline }, s), `regressions:${window}:${baseline}`);
  const r = list.data;
  return (
    <TableCard
      title="Regressions"
      count={r?.items.length}
      description={r ? <>Recent {new Date(r.recent_from).toLocaleTimeString()}–{new Date(r.recent_to).toLocaleTimeString()} against a baseline ending <When iso={r.baseline_to} />. Needs at least {r.min_calls} calls, {r.min_ratio}× and {fmtMs(r.min_delta_ms)} more.</> : "Statements that got slower, or started failing, against their own history."}
      actions={<span className="flex gap-2"><Sel aria-label="Recent window" value={window} onChange={setWindow} options={WINDOWS} /><Sel aria-label="Baseline" value={baseline} onChange={setBaseline} options={BASELINES.map((b) => ({ value: b.value, label: `Against ${b.label}` }))} /><Button onClick={list.reload}>Refresh</Button></span>}
    >
      <CardBody><ErrorView error={list.error} onRetry={list.reload} /></CardBody>
      {list.loading && !r && <CardBody><SkeletonTable rows={3} columns={6} label="Loading regressions" /></CardBody>}
      {r && r.items.length === 0 && <CardBody><EmptyState compact title="No regressions" why="No statement is meaningfully slower or more error-prone than before. That is the good answer." /></CardBody>}
      {r && r.items.length > 0 && (
        <Table framed density="compact" aria-label="Regressions">
          <Table.Header className={HEAD_ROW}>
            <Table.Row><Table.Head>Statement</Table.Head><Table.Head>What changed</Table.Head><Table.Head className={NUM}>Now (p95)</Table.Head><Table.Head className={NUM}>Before (p95)</Table.Head><Table.Head className={NUM}>Change</Table.Head><Table.Head className={NUM}>Extra time</Table.Head><Table.Head className={NUM}>Calls now</Table.Head></Table.Row>
          </Table.Header>
          <Table.Body>
            {r.items.map((x) => (
              <Table.Row key={`${x.kind}:${x.fingerprint}`}>
                <Table.Cell className="max-w-md py-2"><Link to={statementHref(x.fingerprint)} title={x.statement} className="block truncate font-mono text-xs text-primary underline-offset-2 hover:underline">{oneLine(x.statement)}</Link><span className="text-xs text-muted-foreground">{x.area}</span></Table.Cell>
                <Table.Cell><Badge tone={x.kind === "errors" ? "bad" : "warn"}>{x.kind === "errors" ? "errors" : "slower"}</Badge>{x.kind === "errors" && <span className="ml-2 text-xs text-muted-foreground">{fmtRate(x.baseline.error_rate)} → {fmtRate(x.recent.error_rate)}</span>}</Table.Cell>
                <Table.Cell className={NUM}>{fmtMs(x.recent.p95_ms)}</Table.Cell>
                <Table.Cell className={NUM}>{fmtMs(x.baseline.p95_ms)}</Table.Cell>
                <Table.Cell className={`${NUM} font-medium`}>×{Math.round(x.ratio * 10) / 10}</Table.Cell>
                <Table.Cell className={NUM}>{fmtMs(x.extra_ms)}</Table.Cell>
                <Table.Cell className={NUM}>{fmtInt(x.recent.calls)}</Table.Cell>
              </Table.Row>
            ))}
          </Table.Body>
        </Table>
      )}
    </TableCard>
  );
}
