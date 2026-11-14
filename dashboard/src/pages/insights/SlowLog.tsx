import { useState, type FormEvent } from "react";
import { Link } from "react-router-dom";
import { Table } from "panelui-web/table";
import { Input } from "panelui-web/input";
import { listSlowStatements } from "../../api";
import type { SlowStatement } from "../../api";
import { EmptyState } from "../../components/EmptyState";
import { Pager } from "../../components/Pager";
import { Sel } from "../../components/controls";
import { SkeletonTable } from "../../components/Skeleton";
import { Badge } from "../../components/StatusBadge";
import { CardBody, HEAD_ROW, NUM, TableCard } from "../../components/TableCard";
import { Button, ErrorView } from "../../components/ui";
import { useCursorList } from "../../hooks";
import { When } from "../jobs/ops";
import { AREAS, RANGES, fmtInt, fmtMs, oneLine, rangeOf, statementHref } from "./util";

/** Individual statements over the slow threshold, newest first, with their values redacted. Read a page at a time. */
export default function SlowLog() {
  const [range, setRange] = useState("24h");
  const [area, setArea] = useState("");
  const [minMs, setMinMs] = useState("");
  const [draft, setDraft] = useState("");
  const [q, setQ] = useState("");
  const min = minMs.trim() === "" ? undefined : Number(minMs);
  const minBad = min !== undefined && !(Number.isFinite(min) && min >= 0);
  const list = useCursorList<SlowStatement>(
    (cursor, signal) =>
      listSlowStatements({ ...rangeOf(range), area: area || undefined, min_ms: minBad ? undefined : min, q: q || undefined, cursor, limit: 50 }, signal).then((p) => ({ items: p.items, next: p.next_cursor === null ? null : String(p.next_cursor) })),
    `slow:${range}:${area}:${minBad ? "" : min}:${q}`,
  );
  const apply = (e: FormEvent) => { e.preventDefault(); setQ(draft.trim()); };
  return (
    <TableCard
      title="Slow statements"
      count={list.items.length}
      description="Every statement over the slow threshold, newest first. Values are never kept, only the shape."
      actions={<Button onClick={list.reload}>Refresh</Button>}
      toolbar={
        <form onSubmit={apply} role="search" aria-label="Filter slow statements" className="flex flex-wrap items-center gap-2">
          <Sel aria-label="Time range" value={range} onChange={setRange} options={RANGES.map((r) => ({ value: r.id, label: r.label }))} />
          <Sel aria-label="Area" value={area} onChange={setArea} options={[{ value: "", label: "Any area" }, ...AREAS]} />
          <Input size="sm" aria-label="At least (ms)" aria-invalid={minBad || undefined} placeholder="At least ms" inputMode="decimal" className="w-32" value={minMs} onChange={(e) => setMinMs(e.target.value)} />
          <Input size="sm" aria-label="Search statement text" placeholder="Search text" className="w-56" value={draft} onChange={(e) => setDraft(e.target.value)} />
          <Button type="submit">Search</Button>
        </form>
      }
      footer={list.items.length > 0 ? <Pager mode="more" shown={list.items.length} hasMore={list.hasMore} loading={list.loadingMore} onMore={list.loadMore} noun="slow statements" /> : undefined}
    >
      <CardBody><ErrorView error={list.error} onRetry={list.reload} /></CardBody>
      {list.loading && <CardBody><SkeletonTable rows={4} columns={6} label="Loading slow statements" /></CardBody>}
      {!list.loading && !list.error && list.items.length === 0 && <CardBody><EmptyState compact title="No slow statements" why="Nothing ran longer than the threshold in this range. Lower observability.slow_query_ms to catch more." /></CardBody>}
      {list.items.length > 0 && (
        <Table framed density="compact" aria-label="Slow statements">
          <Table.Header className={HEAD_ROW}>
            <Table.Row><Table.Head>When</Table.Head><Table.Head className={NUM}>Took</Table.Head><Table.Head>Statement</Table.Head><Table.Head>Area</Table.Head><Table.Head className={NUM}>Rows</Table.Head><Table.Head>Flags</Table.Head></Table.Row>
          </Table.Header>
          <Table.Body>
            {list.items.map((s) => (
              <Table.Row key={s.id}>
                <Table.Cell><When iso={s.at} /></Table.Cell>
                <Table.Cell className={`${NUM} font-medium`}>{fmtMs(s.duration_ms)}</Table.Cell>
                <Table.Cell className="max-w-md py-2"><Link to={statementHref(s.fingerprint)} title={s.statement} className="block truncate font-mono text-xs text-primary underline-offset-2 hover:underline">{oneLine(s.statement)}</Link></Table.Cell>
                <Table.Cell><Badge>{s.area}</Badge></Table.Cell>
                <Table.Cell className={NUM}>{fmtInt(s.rows)}</Table.Cell>
                <Table.Cell><span className="flex gap-1">{s.error && <Badge tone="bad">error</Badge>}{s.write && <Badge tone="info">write</Badge>}</span></Table.Cell>
              </Table.Row>
            ))}
          </Table.Body>
        </Table>
      )}
    </TableCard>
  );
}
