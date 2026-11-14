import { useState, type FormEvent } from "react";
import { Table } from "panelui-web/table";
import { Input } from "panelui-web/input";
import { auditExportUrl, auditSummary, searchAudit } from "../../api";
import type { InsightAuditEntry, InsightAuditFilter } from "../../api";
import { EmptyState } from "../../components/EmptyState";
import { FormField } from "../../components/FormField";
import { Pager } from "../../components/Pager";
import { Sel } from "../../components/controls";
import { SkeletonTable } from "../../components/Skeleton";
import { Badge } from "../../components/StatusBadge";
import { CardBody, HEAD_ROW, TableCard } from "../../components/TableCard";
import { Button, ErrorView } from "../../components/ui";
import { useCursorList, useLoad } from "../../hooks";
import { Section, When } from "../jobs/ops";
import AuditRelated from "./AuditRelated";
import { BLANK_AUDIT_DRAFT, auditFilterOf, fmtInt, resourceOf, type AuditDraft } from "./util";

/** Audit investigation: search the authentication and administration log, follow an entry to what happened around it, export what matches. */
export default function Audit() {
  const [draft, setDraft] = useState<AuditDraft>(BLANK_AUDIT_DRAFT);
  const [filter, setFilter] = useState<InsightAuditFilter>({});
  const [open, setOpen] = useState<InsightAuditEntry>();
  const key = JSON.stringify(filter);
  const list = useCursorList<InsightAuditEntry>(
    (cursor, signal) => searchAudit({ ...filter, cursor, limit: 50 }, signal).then((p) => ({ items: p.items, next: p.next_cursor === null ? null : String(p.next_cursor) })),
    `audit:${key}`,
  );
  const summary = useLoad((s) => auditSummary({ from: filter.from, to: filter.to }, s), `audit-summary:${filter.from}:${filter.to}`);
  const set = (patch: Partial<AuditDraft>) => setDraft((d) => ({ ...d, ...patch }));
  const submit = (e: FormEvent) => { e.preventDefault(); setFilter(auditFilterOf(draft)); };
  const failures = summary.data?.outcomes.find((o) => o.key === "failure")?.count ?? 0;

  return (
    <div className="space-y-4">
      <Section title="Search the audit trail" description="Sign-ins, key and user changes and other administration events. Every filter is optional and they combine.">
        <form onSubmit={submit} role="search" aria-label="Search audit entries" className="space-y-3">
          <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-4">
            <FormField label="Actor" help="user:users/42, admin:…, service_account:…">{(p) => <Input {...p} size="sm" className="w-full" value={draft.actor} onChange={(e) => set({ actor: e.target.value })} />}</FormField>
            <FormField label="Action" help="Starts with: login, api_key.">{(p) => <Input {...p} size="sm" className="w-full" value={draft.action} onChange={(e) => set({ action: e.target.value })} />}</FormField>
            <FormField label="Resource type">{(p) => <Input {...p} size="sm" className="w-full" value={draft.resource_type} onChange={(e) => set({ resource_type: e.target.value })} />}</FormField>
            <FormField label="Resource id">{(p) => <Input {...p} size="sm" className="w-full" value={draft.resource_id} onChange={(e) => set({ resource_id: e.target.value })} />}</FormField>
            <FormField label="Outcome">{(p) => <Sel {...p} size="sm" className="w-full" value={draft.outcome} onChange={(outcome) => set({ outcome })} options={[{ value: "", label: "Any outcome" }, "success", "failure"]} />}</FormField>
            <FormField label="IP address">{(p) => <Input {...p} size="sm" className="w-full" value={draft.ip} onChange={(e) => set({ ip: e.target.value })} />}</FormField>
            <FormField label="Request id">{(p) => <Input {...p} size="sm" className="w-full" value={draft.request_id} onChange={(e) => set({ request_id: e.target.value })} />}</FormField>
            <FormField label="Contains text" help="In the details, event, resource id or user agent.">{(p) => <Input {...p} size="sm" className="w-full" value={draft.q} onChange={(e) => set({ q: e.target.value })} />}</FormField>
            <FormField label="From" help="Your local time.">{(p) => <Input {...p} size="sm" type="datetime-local" className="w-full" value={draft.from} onChange={(e) => set({ from: e.target.value })} />}</FormField>
            <FormField label="To" help="Before this time.">{(p) => <Input {...p} size="sm" type="datetime-local" className="w-full" value={draft.to} onChange={(e) => set({ to: e.target.value })} />}</FormField>
            <FormField label="Order">{(p) => <Sel {...p} size="sm" className="w-full" value={draft.order} onChange={(order) => set({ order })} options={[{ value: "desc", label: "Newest first" }, { value: "asc", label: "Oldest first" }]} />}</FormField>
          </div>
          <div className="flex flex-wrap items-center gap-2">
            <Button type="submit" tone="primary">Search</Button>
            <Button type="button" onClick={() => { setDraft(BLANK_AUDIT_DRAFT); setFilter({}); }}>Clear</Button>
          </div>
        </form>
      </Section>

      {summary.data && (
        <p role="status" className="text-sm text-muted-foreground">
          {fmtInt(summary.data.total)} entr{summary.data.total === 1 ? "y" : "ies"} in this period{failures > 0 ? <>, <span className="text-destructive">{fmtInt(failures)} failed</span></> : ""}.
        </p>
      )}

      <TableCard
        title="Results"
        count={list.items.length}
        description={filter.order === "asc" ? "Oldest first." : "Newest first."}
        actions={
          <span className="flex flex-wrap items-center gap-2" role="group" aria-label="Export what matches">
            <a href={auditExportUrl(filter, "csv")} download className="inline-flex h-8 items-center rounded-lg border border-border px-3 text-sm font-medium hover:bg-muted/40">Export CSV</a>
            <a href={auditExportUrl(filter, "jsonl")} download className="inline-flex h-8 items-center rounded-lg border border-border px-3 text-sm font-medium hover:bg-muted/40">Export JSON lines</a>
          </span>
        }
        footer={list.items.length > 0 ? <Pager mode="more" shown={list.items.length} hasMore={list.hasMore} loading={list.loadingMore} onMore={list.loadMore} noun="entries" /> : undefined}
      >
        <CardBody>
          <ErrorView error={list.error} onRetry={list.reload} />
          <p className="text-xs text-muted-foreground">An export streams every match, oldest first, up to 100,000 rows. Cells that start like a spreadsheet formula are prefixed with ' so they open as text.</p>
        </CardBody>
        {list.loading && <CardBody><SkeletonTable rows={4} columns={6} label="Loading audit entries" /></CardBody>}
        {!list.loading && !list.error && list.items.length === 0 && <CardBody><EmptyState compact title="No entries match" why="Widen the time range or clear a filter." /></CardBody>}
        {list.items.length > 0 && (
          <Table framed density="compact" aria-label="Audit entries">
            <Table.Header className={HEAD_ROW}>
              <Table.Row><Table.Head>When</Table.Head><Table.Head>Event</Table.Head><Table.Head>Outcome</Table.Head><Table.Head>Actor</Table.Head><Table.Head>Resource</Table.Head><Table.Head>IP</Table.Head><Table.Head><span className="sr-only">Actions</span></Table.Head></Table.Row>
            </Table.Header>
            <Table.Body>
              {list.items.map((e) => (
                <Table.Row key={e.id}>
                  <Table.Cell><When iso={e.at} /></Table.Cell>
                  <Table.Cell className="font-mono text-xs">{e.event}</Table.Cell>
                  <Table.Cell><Badge status={e.outcome}>{e.outcome}</Badge></Table.Cell>
                  <Table.Cell className="max-w-48 truncate" title={e.actor}>{e.actor}{e.impersonator && <span className="block text-xs text-muted-foreground">via {e.impersonator}</span>}</Table.Cell>
                  <Table.Cell className="max-w-48 truncate text-muted-foreground" title={resourceOf(e)}>{resourceOf(e)}</Table.Cell>
                  <Table.Cell className="font-mono text-xs">{e.ip ?? "—"}</Table.Cell>
                  <Table.Cell className="text-right"><Button aria-label={`Related events of entry ${e.id}`} onClick={() => setOpen(e)}>Related</Button></Table.Cell>
                </Table.Row>
              ))}
            </Table.Body>
          </Table>
        )}
      </TableCard>
      {open && <AuditRelated entry={open} onClose={() => setOpen(undefined)} />}
    </div>
  );
}
