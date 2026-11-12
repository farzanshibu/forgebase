import { ArrowRight01Icon } from "@hugeicons/core-free-icons";
import { HugeiconsIcon } from "@hugeicons/react";
import { Input } from "panelui-web/input";
import { Table } from "panelui-web/table";
import { Tabs } from "panelui-web/tabs";
import { useId, useState, type ReactNode } from "react";
import { Link } from "react-router-dom";
import { authAudit, authzAudit, type AuditEntry, type AuthzAuditRow } from "../../api";
import { DetailSheet } from "../../components/CreateSheet";
import { EmptyState } from "../../components/EmptyState";
import { ErrorPanel } from "../../components/ErrorPanel";
import { Pager } from "../../components/Pager";
import { SkeletonTable } from "../../components/Skeleton";
import { Button } from "../../components/ui";
import { useLoad } from "../../hooks";
import { PrincipalText } from "./common";
import { EVENT_PREFIXES, agentLabel, eventLabel, fmtTime, useAction, useAdminEmails } from "./words";
import { Badge } from "../../components/StatusBadge";
import { CardBody, HEAD_ROW, TableCard, Th } from "../../components/TableCard";
import { IdText } from "../../components/IdText";
import { When } from "../../components/Time";

const PAGE = 50;

/** The event's target as a principal string, to compare with its actor. */
const subjectOf = (r: AuditEntry) => `${r.subject_type ?? ""}:${r.collection ? `${r.collection}/` : ""}${r.subject_id ?? ""}`;

export default function AuditLog() {
  const [kind, setKind] = useState<"auth" | "authz">("auth");
  const admins = useAdminEmails();
  return (
    <div className="space-y-4">
      <Tabs variant="segmented" value={kind} onValueChange={(v) => setKind(v as "auth" | "authz")}>
        <Tabs.List aria-label="Audit logs">
          <Tabs.Trigger value="auth">Authentication</Tabs.Trigger>
          <Tabs.Trigger value="authz">Authorization</Tabs.Trigger>
        </Tabs.List>
      </Tabs>
      {kind === "auth" ? <AuthEvents admins={admins} /> : <AuthzEvents admins={admins} />}
    </div>
  );
}

/** "via <administrator>" when an administrator did it while impersonating. */
function Impersonator({ id, admins }: { id?: string | null; admins: Map<string, string> }) {
  if (!id) return null;
  return (
    <span className="flex items-center gap-1 text-xs text-muted-foreground" title={`Done by administrator ${id} while impersonating`}>
      via <Badge tone="warn">{admins.get(id) ?? id}</Badge>
    </span>
  );
}

function AuthEvents({ admins }: { admins: Map<string, string> }) {
  const [event, setEvent] = useState("");
  const [subject, setSubject] = useState("");
  const [applied, setApplied] = useState({ event: "", subject: "" });
  const [open, setOpen] = useState<AuditEntry>();
  const listId = `ev${useId().replace(/:/g, "")}`;
  const first = useLoad((s) => authAudit({ event: applied.event, subject_id: applied.subject }, s), `audit:${applied.event}:${applied.subject}`);
  const [older, setOlder] = useState<{ key: string; rows: AuditEntry[]; done: boolean }>();
  const key = `${applied.event}:${applied.subject}`;
  const more = older?.key === key ? older : undefined;
  const rows = [...(first.data ?? []), ...(more?.rows ?? [])];
  const hasMore = more ? !more.done : (first.data?.length ?? 0) >= PAGE;
  const filtered = Boolean(applied.event || applied.subject);
  const act = useAction();
  const loadOlder = async () => {
    const last = rows[rows.length - 1];
    if (!last) return;
    await act.run(async () => {
      const page = await authAudit({ event: applied.event, subject_id: applied.subject, before: last.id });
      setOlder({ key, rows: [...(more?.rows ?? []), ...page], done: page.length < PAGE });
    });
  };
  return (
    <TableCard
      title="Sign-in and account events"
      description="Sign-ins, sign-outs, MFA and password changes of users and administrators, newest first. Rows on a yellow tint were done while impersonating."
      toolbar={
        <form
          role="search"
          aria-label="Filter events"
          className="flex w-full flex-wrap items-center gap-2"
          onSubmit={(e) => {
            e.preventDefault();
            setApplied({ event: event.trim(), subject: subject.trim() });
          }}
        >
          <Input size="sm" className="w-56" aria-label="Event prefix" placeholder="Event, such as login or mfa." list={listId} value={event} onChange={(e) => setEvent(e.target.value)} />
          <datalist id={listId}>
            {EVENT_PREFIXES.map((p) => <option key={p} value={p}>{eventLabel(p.replace(/\.$/, ""))}</option>)}
          </datalist>
          <Input size="sm" className="w-56 font-mono" aria-label="Subject id" placeholder="Subject id" autoComplete="off" spellCheck={false} value={subject} onChange={(e) => setSubject(e.target.value)} />
          <Button type="submit">Filter</Button>
          {filtered && (
            <Button type="button" onClick={() => { setEvent(""); setSubject(""); setApplied({ event: "", subject: "" }); }}>
              Clear
            </Button>
          )}
        </form>
      }
      footer={rows.length > 0 ? <Pager mode="more" shown={rows.length} hasMore={hasMore} loading={act.busy} onMore={() => void loadOlder()} noun="events" label="Older events" /> : undefined}
    >
      {!!first.error && <CardBody><ErrorPanel error={first.error} onRetry={first.reload} /></CardBody>}
      {!!act.error && <CardBody>{act.errorView}</CardBody>}
      {first.loading && !first.data && !first.error && <CardBody><SkeletonTable rows={6} columns={6} label="Loading audit log" /></CardBody>}
      {first.data && rows.length === 0 && (
        <CardBody>
          {filtered ? <EmptyState compact title="No events match" why="Try a shorter event prefix, such as login, or clear the filters." /> : <EmptyState compact title="No events yet" why="Sign-ins, sign-outs and account changes are recorded here as they happen." />}
        </CardBody>
      )}
      {rows.length > 0 && (
        <Table framed density="compact" className="text-sm" aria-label="Authentication events">
          <Table.Header className={HEAD_ROW}>
            <Table.Row><Th>When</Th><Th>Actor</Th><Th>Event</Th><Th>Outcome</Th><Th>Target</Th><Table.Head className="hidden xl:table-cell">Source</Table.Head><Th><span className="sr-only">Details</span></Th></Table.Row>
          </Table.Header>
          <Table.Body>
            {rows.map((r) => (
              <Table.Row key={r.id} className={r.impersonator ? "bg-warning/5" : ""}>
                <Table.Cell className="text-muted-foreground"><When value={r.at} /></Table.Cell>
                <Table.Cell className="max-w-56">
                  <PrincipalText value={r.actor} admins={admins} />
                  <Impersonator id={r.impersonator} admins={admins} />
                </Table.Cell>
                <Table.Cell>
                  <span className="flex flex-col">
                    <span className="whitespace-nowrap">{eventLabel(r.event)}</span>
                    <code className="font-mono text-xs text-muted-foreground">{r.event}</code>
                  </span>
                </Table.Cell>
                <Table.Cell>{r.outcome === "failure" ? <Badge tone="bad" dot>Failure</Badge> : <Badge tone="ok" dot>Success</Badge>}</Table.Cell>
                <Table.Cell className="max-w-52">
                  {!r.subject_id ? (
                    <span className="text-muted-foreground">—</span>
                  ) : subjectOf(r) === r.actor ? (
                    <span className="text-xs text-muted-foreground" title={r.actor}>Self</span>
                  ) : (
                    <span className="flex min-w-0 items-center gap-1.5" title={subjectOf(r)}>
                      {r.subject_type && <span className="shrink-0 text-xs capitalize text-muted-foreground">{r.subject_type}</span>}
                      <span className="truncate font-mono text-xs">{r.subject_type === "admin" ? admins.get(r.subject_id) ?? r.subject_id : `${r.collection ? `${r.collection}/` : ""}${r.subject_id}`}</span>
                    </span>
                  )}
                </Table.Cell>
                <Table.Cell className="hidden max-w-48 xl:table-cell">
                  <span className="flex flex-col">
                    <span className="font-mono text-xs">{r.ip ?? "—"}</span>
                    {r.user_agent && <span className="truncate text-xs text-muted-foreground" title={r.user_agent}>{agentLabel(r.user_agent)}</span>}
                  </span>
                </Table.Cell>
                <Table.Cell align="end">
                  <Button aria-label={`Details of event ${r.id}`} title="Details" className="size-8 px-0" onClick={() => setOpen(r)}>
                    <HugeiconsIcon icon={ArrowRight01Icon} size={16} aria-hidden="true" />
                  </Button>
                </Table.Cell>
              </Table.Row>
            ))}
          </Table.Body>
        </Table>
      )}
      <DetailSheet open={!!open} onOpenChange={(v) => { if (!v) setOpen(undefined); }} title={open ? eventLabel(open.event) : ""} description={open ? fmtTime(open.at) : undefined}>
        {open && <EventDetail e={open} admins={admins} />}
      </DetailSheet>
    </TableCard>
  );
}

function EventDetail({ e, admins }: { e: AuditEntry; admins: Map<string, string> }) {
  const facts: [string, ReactNode][] = [
    ["Event", <code key="e" className="font-mono text-xs">{e.event}</code>],
    ["Outcome", e.outcome === "failure" ? <Badge tone="bad" dot>Failure</Badge> : <Badge tone="ok" dot>Success</Badge>],
    ["Actor", <PrincipalText key="a" value={e.actor} admins={admins} />],
    ["Target", e.subject_id ? <code key="t" className="font-mono text-xs">{[e.subject_type, e.collection, e.subject_id].filter(Boolean).join(" · ")}</code> : "—"],
    ["IP address", <code key="i" className="font-mono text-xs">{e.ip ?? "—"}</code>],
    ["Client", e.user_agent ? <span key="u" className="break-all text-xs">{e.user_agent}</span> : "—"],
    ["Impersonated by", e.impersonator ? admins.get(e.impersonator) ?? e.impersonator : "—"],
    ["Request id", e.request_id ? (
      <span key="r" className="flex flex-wrap items-center gap-x-3">
        <IdText id={e.request_id} what="request id" />
        <Link to={`/functions/logs?request=${encodeURIComponent(e.request_id)}`} className="text-xs font-medium text-primary underline-offset-2 hover:underline">Find in logs</Link>
      </span>
    ) : "—"],
  ];
  const hasDetail = !!e.detail && typeof e.detail === "object" && Object.keys(e.detail).length > 0;
  return (
    <div className="space-y-4 text-sm">
      <dl className="divide-y divide-border rounded-lg border border-border">
        {facts.map(([k, v]) => (
          <div key={k} className="grid grid-cols-[8rem_1fr] items-center gap-3 px-3 py-2">
            <dt className="text-xs text-muted-foreground">{k}</dt>
            <dd className="min-w-0">{v}</dd>
          </div>
        ))}
      </dl>
      {hasDetail && (
        <div>
          <p className="mb-1 text-xs font-medium text-muted-foreground">Details</p>
          <pre className="overflow-x-auto rounded-lg border border-border bg-muted/50 p-3 font-mono text-xs leading-relaxed">{JSON.stringify(e.detail, null, 2)}</pre>
        </div>
      )}
    </div>
  );
}

function AuthzEvents({ admins }: { admins: Map<string, string> }) {
  const { data, error, loading, reload } = useLoad((s) => authzAudit(100, s), "authz-audit");
  return (
    <TableCard
      title="Denied requests"
      count={data?.length}
      description="The latest 100 requests the access rules refused, newest first, with the step of the pipeline that said no."
    >
      {!!error && <CardBody><ErrorPanel error={error} onRetry={reload} /></CardBody>}
      {loading && !data && !error && <CardBody><SkeletonTable rows={4} columns={6} label="Loading authorization denials" /></CardBody>}
      {data?.length === 0 && (
        <CardBody>
          <EmptyState compact title="No denied requests" why="When a role, an ACL or a rule refuses a request, it shows here with the reason. Test a rule in Access › Rule tester." />
        </CardBody>
      )}
      {!!data?.length && (
        <Table framed density="compact" className="text-sm" aria-label="Denied requests">
          <Table.Header className={HEAD_ROW}>
            <Table.Row><Th>When</Th><Th>Principal</Th><Th>Permission</Th><Th>Resource</Th><Th>Step</Th><Th>Reason</Th></Table.Row>
          </Table.Header>
          <Table.Body>
            {data.map((r: AuthzAuditRow, i) => (
              <Table.Row key={`${r.at}-${i}`} className={r.impersonator ? "bg-warning/5" : ""}>
                <Table.Cell className="text-muted-foreground"><When value={r.at} /></Table.Cell>
                <Table.Cell className="max-w-60">
                  <PrincipalText value={r.principal} admins={admins} />
                  <Impersonator id={r.impersonator} admins={admins} />
                </Table.Cell>
                <Table.Cell><code className="font-mono text-xs">{r.permission}</code></Table.Cell>
                <Table.Cell className="max-w-48 truncate font-mono text-xs" title={r.resource ?? undefined}>{r.resource ?? <span className="font-sans text-muted-foreground">—</span>}</Table.Cell>
                <Table.Cell><Badge tone="bad">{r.step}</Badge></Table.Cell>
                <Table.Cell className="max-w-72 text-muted-foreground">{r.reason}</Table.Cell>
              </Table.Row>
            ))}
          </Table.Body>
        </Table>
      )}
    </TableCard>
  );
}
