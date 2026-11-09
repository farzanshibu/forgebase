import { useState } from "react";
import { authAudit, authzAudit, type AuditEntry } from "../../api";
import { Button, ErrorView, Loading, inputCls } from "../../components/ui";
import { useLoad } from "../../hooks";
import { Badge, THEAD, Th, fmtTime, useAction } from "./common";

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
            className={`rounded-md border px-3 py-1.5 text-sm ${kind === k ? "border-amber-500 text-white" : "border-zinc-700 text-zinc-400"}`}
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
  return v ? <Badge tone="warn">{v}</Badge> : <span className="text-zinc-600">—</span>;
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
        <input aria-label="Event prefix" className={inputCls} placeholder="Event prefix (login, mfa.)" value={event} onChange={(e) => setEvent(e.target.value)} />
        <input aria-label="Subject id" className={inputCls} placeholder="Subject id" value={subject} onChange={(e) => setSubject(e.target.value)} />
        <Button type="submit">Filter</Button>
      </form>
      <ErrorView error={first.error} />
      {act.errorView}
      {first.loading && !first.error && <Loading what="Loading audit log" />}
      {first.data && rows.length === 0 && <p className="text-sm text-zinc-400">No audit events match.</p>}
      {rows.length > 0 && (
        <table className="w-full text-sm">
          <thead className={THEAD}>
            <tr><Th>When</Th><Th>Event</Th><Th>Outcome</Th><Th>Actor</Th><Th>Subject</Th><Th>IP</Th><Th>Impersonator</Th></tr>
          </thead>
          <tbody>
            {rows.map((r) => (
              <tr key={r.id} className={`border-t border-zinc-800 ${r.impersonator ? "bg-amber-500/5" : ""}`}>
                <td className="py-1.5 pr-3 whitespace-nowrap">{fmtTime(r.at)}</td>
                <td className="pr-3">{r.event}</td>
                <td className="pr-3">{r.outcome === "failure" ? <Badge tone="bad">failure</Badge> : <Badge tone="ok">success</Badge>}</td>
                <td className="pr-3 font-mono text-xs">{r.actor}</td>
                <td className="pr-3 font-mono text-xs">{r.subject_id ?? "—"}</td>
                <td className="pr-3 font-mono text-xs">{r.ip ?? "—"}</td>
                <td className="pr-3"><Imp v={r.impersonator} /></td>
              </tr>
            ))}
          </tbody>
        </table>
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
      {data?.length === 0 && <p className="text-sm text-zinc-400">No denials recorded.</p>}
      {!!data?.length && (
        <table className="w-full text-sm">
          <thead className={THEAD}>
            <tr><Th>When</Th><Th>Step</Th><Th>Permission</Th><Th>Principal</Th><Th>Resource</Th><Th>Reason</Th><Th>Impersonator</Th></tr>
          </thead>
          <tbody>
            {data.map((r, i) => (
              <tr key={`${r.at}-${i}`} className={`border-t border-zinc-800 ${r.impersonator ? "bg-amber-500/5" : ""}`}>
                <td className="py-1.5 pr-3 whitespace-nowrap">{fmtTime(r.at)}</td>
                <td className="pr-3"><Badge tone="bad">{r.step}</Badge></td>
                <td className="pr-3 font-mono text-xs">{r.permission}</td>
                <td className="pr-3 font-mono text-xs">{r.principal}</td>
                <td className="pr-3 font-mono text-xs">{r.resource ?? "—"}</td>
                <td className="pr-3 text-zinc-400">{r.reason}</td>
                <td className="pr-3"><Imp v={r.impersonator} /></td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </div>
  );
}
