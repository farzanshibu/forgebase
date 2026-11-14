import { useState, type FormEvent } from "react";
import { Link } from "react-router-dom";
import { Table } from "panelui-web/table";
import { Input } from "panelui-web/input";
import { STATEMENT_SORTS, listStatements } from "../../api";
import type { StatementSort } from "../../api";
import { EmptyState } from "../../components/EmptyState";
import { HelpTip } from "../../components/HelpTip";
import { Pager } from "../../components/Pager";
import { Sel } from "../../components/controls";
import { SkeletonTable } from "../../components/Skeleton";
import { Badge } from "../../components/StatusBadge";
import { CardBody, HEAD_ROW, NUM, TableCard } from "../../components/TableCard";
import { Button, ErrorView } from "../../components/ui";
import { useLoad } from "../../hooks";
import { AREAS, RANGES, fmtInt, fmtMs, oneLine, rangeOf, statementHref } from "./util";

const PER_PAGE = 25;
const SORT_WORDS: Record<StatementSort, string> = {
  total_time: "Total time", calls: "Calls", mean: "Mean time", max: "Slowest", p50: "p50", p95: "p95", p99: "p99", errors: "Errors", rows: "Rows", slow: "Slow runs",
};

/** The statement shapes that cost the most, by total time by default, with their percentiles. */
export default function Statements() {
  const [range, setRange] = useState("1h");
  const [area, setArea] = useState("");
  const [sort, setSort] = useState<StatementSort>("total_time");
  const [draft, setDraft] = useState("");
  const [q, setQ] = useState("");
  const [page, setPage] = useState(1);
  const list = useLoad((s) => listStatements({ ...rangeOf(range), area: area || undefined, q: q || undefined, sort, limit: PER_PAGE, offset: (page - 1) * PER_PAGE }, s), `statements:${range}:${area}:${q}:${sort}:${page}`);
  const reset = <T,>(set: (v: T) => void) => (v: T) => { set(v); setPage(1); };
  const apply = (e: FormEvent) => { e.preventDefault(); setQ(draft.trim()); setPage(1); };

  return (
    <TableCard
      title="Statements"
      count={list.data?.total}
      description={<>Each row is one statement <HelpTip term="fingerprint">shape</HelpTip>, with its values removed. Percentiles are within about 11% of the true value.</>}
      actions={<Button onClick={list.reload}>Refresh</Button>}
      toolbar={
        <form onSubmit={apply} className="flex flex-wrap items-center gap-2" role="search" aria-label="Filter statements">
          <Sel aria-label="Time range" value={range} onChange={reset(setRange)} options={RANGES.map((r) => ({ value: r.id, label: r.label }))} />
          <Sel aria-label="Area" value={area} onChange={reset(setArea)} options={[{ value: "", label: "Any area" }, ...AREAS]} />
          <Sel aria-label="Sort by" value={sort} onChange={reset(setSort)} options={STATEMENT_SORTS.map((s) => ({ value: s, label: `By ${SORT_WORDS[s].toLowerCase()}` }))} />
          <Input size="sm" aria-label="Search statement text" placeholder="Search text" className="w-56" value={draft} onChange={(e) => setDraft(e.target.value)} />
          <Button type="submit">Search</Button>
        </form>
      }
      footer={list.data && list.data.total > PER_PAGE ? <Pager page={page} perPage={PER_PAGE} total={list.data.total} onPage={setPage} label="Statements pages" /> : undefined}
    >
      <CardBody><ErrorView error={list.error} onRetry={list.reload} /></CardBody>
      {list.loading && !list.data && <CardBody><SkeletonTable rows={5} columns={8} label="Loading statements" /></CardBody>}
      {list.data && list.data.items.length === 0 && <CardBody><EmptyState compact title="No statements in this range" why="Statistics are written every flush interval. Run some requests, then use Flush statistics now, or widen the range." /></CardBody>}
      {list.data && list.data.items.length > 0 && (
        <Table framed density="compact" aria-label="Statements by total time">
          <Table.Header className={HEAD_ROW}>
            <Table.Row><Table.Head>Statement</Table.Head><Table.Head>Area</Table.Head><Table.Head className={NUM}>Calls</Table.Head><Table.Head className={NUM}>Errors</Table.Head><Table.Head className={NUM}>p50</Table.Head><Table.Head className={NUM}>p95</Table.Head><Table.Head className={NUM}>p99</Table.Head><Table.Head className={NUM}>Total</Table.Head></Table.Row>
          </Table.Header>
          <Table.Body>
            {list.data.items.map((s) => (
              <Table.Row key={s.fingerprint}>
                <Table.Cell className="max-w-md py-2">
                  <Link to={statementHref(s.fingerprint)} title={s.statement} className="block truncate font-mono text-xs text-primary underline-offset-2 hover:underline">{oneLine(s.statement)}</Link>
                </Table.Cell>
                <Table.Cell><Badge>{s.area}</Badge></Table.Cell>
                <Table.Cell className={NUM}>{fmtInt(s.calls)}</Table.Cell>
                <Table.Cell className={NUM}>{s.errors > 0 ? <span className="text-destructive">{fmtInt(s.errors)}</span> : 0}</Table.Cell>
                <Table.Cell className={NUM}>{fmtMs(s.p50_ms)}</Table.Cell>
                <Table.Cell className={NUM}>{fmtMs(s.p95_ms)}</Table.Cell>
                <Table.Cell className={NUM}>{fmtMs(s.p99_ms)}</Table.Cell>
                <Table.Cell className={`${NUM} font-medium`}>{fmtMs(s.total_ms)}</Table.Cell>
              </Table.Row>
            ))}
          </Table.Body>
        </Table>
      )}
    </TableCard>
  );
}
