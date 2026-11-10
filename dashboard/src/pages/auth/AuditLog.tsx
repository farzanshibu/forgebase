import { useState } from "react";
import { authAudit, authzAudit, type AuditEntry } from "../../api";
import { Button, ErrorView, Loading } from "../../components/ui";
import { useLoad } from "../../hooks";
import { Badge, THEAD, Th, fmtTime, useAction } from "./common";
import { Table } from "panelui-web/table";
import { Input } from "panelui-web/input";

export default function AuditLog() {
  const [kind, setKind] = useState<"auth" | "authz">("auth");
  return (
    <div>
      <div className="mb-4 flex gap-1" role="tablist" aria-label="Audit logs">
        {(["auth", "authz"] as const).map((k) => (
          <button
            key={k}
            role="tab"
            aria-selected={kind === k}
            onClick={() => setKind(k)}
            className={`rounded-md border px-3 py-1.5 text-sm ${kind === k ? "border-primary text-foreground" : "border-border text-muted-foreground"}`}
          >
            {k === "auth" ? "Authentication" : "Authorization"}
          </button>
        ))}
      </div>
      {kind === "auth" ? <AuthEvents /> : <AuthzEvents />}
    </div>
  );
}

function Imp({ v }: { v?: string | null }) {
  return v ? <Badge tone="warn">{v}</Badge> : <span className="text-muted-foreground">—</span>;
}

function AuthEvents() {
  const [event, setEvent] = useState("");
  const [subject, setSubject] = useState("");
  const [applied, setApplied] = useState({ event: "", subject: "" });
  const first = useLoad((s) => authAudit({ event: applied.event, subject_id: applied.subject }, s), `audit:${applied.event}:${applied.subject}`);
  const [older, setOlder] = useState<{ key: string; rows: AuditEntry[]; done: boolean }>();
  const key = `${applied.event}:${applied.subject}`;
  const more = older?.key === key ? older : undefined;
  const rows = [...(first.data ?? []), ...(more?.rows ?? [])];
  const act = useAction();
  const loadOlder = async () => {
    const last = rows[rows.length - 1];
    if (!last) return;
    await act.run(async () => {
      const page = await authAudit({ event: applied.event, subject_id: applied.subject, before: last.id });
      setOlder({ key, rows: [...(more?.rows ?? []), ...page], done: page.length < 50 });
    });
  };
  return (
    <div>
      <form
        className="mb-3 flex flex-wrap items-center gap-2"
        onSubmit={(e) => {
          e.preventDefault();
          setApplied({ event: event.trim(), subject: subject.trim() });
        }}
      >
        <Input size="sm" aria-label="Event prefix" placeholder="Event prefix (login, mfa.)" value={event} onChange={(e) => setEvent(e.target.value)} />
        <Input size="sm" aria-label="Subject id" placeholder="Subject id" value={subject} onChange={(e) => setSubject(e.target.value)} />
        <Button type="submit">Filter</Button>
      </form>
      <ErrorView error={first.error} />
      {act.errorView}
      {first.loading && !first.error && <Loading what="Loading audit log" />}
      {first.data && rows.length === 0 && <p className="text-sm text-muted-foreground">No audit events match.</p>}
      {rows.length > 0 && (
        <Table className="w-full text-sm">
          <Table.Header className={THEAD}>
            <Table.Row><Th>When</Th><Th>Event</Th><Th>Outcome</Th><Th>Actor</Th><Th>Subject</Th><Th>IP</Th><Th>Impersonator</Th></Table.Row>
          </Table.Header>
          <Table.Body>
            {rows.map((r) => (
              <Table.Row key={r.id} className={`border-t border-border ${r.impersonator ? "bg-warning/5" : ""}`}>
                <Table.Cell className="py-1.5 pr-3 whitespace-nowrap">{fmtTime(r.at)}</Table.Cell>
                <Table.Cell className="pr-3">{r.event}</Table.Cell>
                <Table.Cell className="pr-3">{r.outcome === "failure" ? <Badge tone="bad">failure</Badge> : <Badge tone="ok">success</Badge>}</Table.Cell>
                <Table.Cell className="pr-3 font-mono text-xs">{r.actor}</Table.Cell>
                <Table.Cell className="pr-3 font-mono text-xs">{r.subject_id ?? "—"}</Table.Cell>
                <Table.Cell className="pr-3 font-mono text-xs">{r.ip ?? "—"}</Table.Cell>
                <Table.Cell className="pr-3"><Imp v={r.impersonator} /></Table.Cell>
              </Table.Row>
            ))}
          </Table.Body>
        </Table>
      )}
      {rows.length > 0 && !more?.done && (
        <div className="mt-3">
          <Button onClick={loadOlder} disabled={act.busy}>
            Load older
          </Button>
        </div>
      )}
    </div>
  );
}

function AuthzEvents() {
  const { data, error, loading } = useLoad((s) => authzAudit(100, s), "authz-audit");
  return (
    <div>
      <ErrorView error={error} />
      {loading && !error && <Loading what="Loading authorization denials" />}
      {data?.length === 0 && <p className="text-sm text-muted-foreground">No denials recorded.</p>}
      {!!data?.length && (
        <Table className="w-full text-sm">
          <Table.Header className={THEAD}>
            <Table.Row><Th>When</Th><Th>Step</Th><Th>Permission</Th><Th>Principal</Th><Th>Resource</Th><Th>Reason</Th><Th>Impersonator</Th></Table.Row>
          </Table.Header>
          <Table.Body>
            {data.map((r, i) => (
              <Table.Row key={`${r.at}-${i}`} className={`border-t border-border ${r.impersonator ? "bg-warning/5" : ""}`}>
                <Table.Cell className="py-1.5 pr-3 whitespace-nowrap">{fmtTime(r.at)}</Table.Cell>
                <Table.Cell className="pr-3"><Badge tone="bad">{r.step}</Badge></Table.Cell>
                <Table.Cell className="pr-3 font-mono text-xs">{r.permission}</Table.Cell>
                <Table.Cell className="pr-3 font-mono text-xs">{r.principal}</Table.Cell>
                <Table.Cell className="pr-3 font-mono text-xs">{r.resource ?? "—"}</Table.Cell>
                <Table.Cell className="pr-3 text-muted-foreground">{r.reason}</Table.Cell>
                <Table.Cell className="pr-3"><Imp v={r.impersonator} /></Table.Cell>
              </Table.Row>
            ))}
          </Table.Body>
        </Table>
      )}
    </div>
  );
}
