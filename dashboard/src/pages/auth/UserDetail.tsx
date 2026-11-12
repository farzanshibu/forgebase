import { Input } from "panelui-web/input";
import { Table } from "panelui-web/table";
import { Textarea } from "panelui-web/textarea";
import { useId, useState } from "react";
import { useParams } from "react-router-dom";
import {
  getUserDetail, listSuspensions, revokeAllSessions, revokeSession, suspend, unsuspend, userLoginHistory, userSessions,
} from "../../api";
import { useSession } from "../../auth/session";
import { Sel } from "../../components/controls";
import { useCrumbs } from "../../components/crumbs";
import { EmptyState } from "../../components/EmptyState";
import { ErrorPanel } from "../../components/ErrorPanel";
import { FormField } from "../../components/FormField";
import { HelpTip } from "../../components/HelpTip";
import { SkeletonLines, SkeletonTable } from "../../components/Skeleton";
import { Button, ConfirmButton } from "../../components/ui";
import { useLoad } from "../../hooks";
import { notify } from "../../toaster";
import { Initials, Modal } from "./common";
import { agentLabel, eventLabel, fmtTime, useAction } from "./words";
import { Badge } from "../../components/StatusBadge";
import { CardBody, HEAD_ROW, TableCard, Th } from "../../components/TableCard";
import { Chip } from "../../components/Chips";
import { Facts } from "../../components/Facts";
import { IdText } from "../../components/IdText";
import { When } from "../../components/Time";

const METHODS: Record<string, string> = { password: "Password", magic_link: "E-mail link", oauth: "Provider", webauthn: "Passkey", email_otp: "E-mail code", sms_otp: "Text message code", anonymous: "Anonymous", refresh: "Refresh" };
const methodLabel = (m: string) => METHODS[m] ?? m.replace(/_/g, " ");

export default function UserDetail() {
  const { collection = "", id = "" } = useParams();
  const principal = `user:${collection}/${id}`;
  const detail = useLoad((s) => getUserDetail(collection, id, s), `ud:${principal}`);
  const sessions = useLoad((s) => userSessions(collection, id, s), `us:${principal}`);
  const history = useLoad((s) => userLoginHistory(collection, id, 50, s), `uh:${principal}`);
  const susp = useLoad((s) => listSuspensions(s), "suspensions");
  const act = useAction();
  const [reason, setReason] = useState("");
  const [showImp, setShowImp] = useState(false);
  const suspension = susp.data?.find((s) => s.principal === principal);
  const user = detail.data?.user;
  const email = user ? String(user.email ?? id) : id;
  useCrumbs(detail.data ? [{ label: "Authentication", to: "/auth" }, { label: "Users", to: "/auth" }, { label: email }] : undefined);

  const reloadAll = () => {
    detail.reload();
    sessions.reload();
  };

  if (detail.error) return <ErrorPanel error={detail.error} onRetry={detail.reload} />;
  if (!detail.data || !user) return <SkeletonLines lines={4} label="Loading user" />;
  const d = detail.data;

  return (
    <div className="space-y-6">
      <header className="flex flex-wrap items-start justify-between gap-4">
        <div className="flex min-w-0 items-center gap-4">
          <Initials name={email} size="lg" />
          <div className="min-w-0">
            <h2 className="truncate text-xl font-semibold tracking-tight">{email}</h2>
            <div className="mt-1 flex flex-wrap items-center gap-2 text-sm text-muted-foreground">
              {suspension ? <Badge status="suspended">Suspended</Badge> : <Badge status="active">Active</Badge>}
              {user.verified ? <Badge tone="ok">E-mail verified</Badge> : <Badge tone="neutral">E-mail not verified</Badge>}
              <span className="inline-flex min-w-0 items-center gap-1">
                <span className="text-xs">in</span> <code className="font-mono text-xs text-foreground/80">{collection}</code>
              </span>
              <IdText id={id} what="user id" />
            </div>
          </div>
        </div>
        <div className="flex items-center gap-2">
          <Button onClick={() => setShowImp(true)}>Impersonate</Button>
        </div>
      </header>

      <Facts
        items={[
          { label: <span className="inline-flex items-center gap-1">MFA <HelpTip term="mfa" /></span>, value: d.mfa_enabled ? <span className="text-success">Enabled</span> : <span className="text-muted-foreground">Not enabled</span> },
          { label: "Active sessions", value: <span className="tabular-nums">{d.active_sessions}</span> },
          { label: "Last sign-in", value: <When value={d.last_login_at} fallback="Never" /> },
          { label: "Created", value: <When value={user.created as string | undefined} /> },
          { label: "Updated", value: <When value={user.updated as string | undefined} /> },
        ]}
      />

      {act.errorView}

      {suspension && (
        <div role="note" className="flex flex-wrap items-center justify-between gap-3 rounded-xl border border-destructive/40 bg-destructive/5 px-4 py-3 text-sm">
          <div>
            <p className="font-medium text-destructive">This account is suspended{suspension.until ? ` until ${fmtTime(suspension.until)}` : ""}.</p>
            <p className="mt-0.5 text-muted-foreground">Reason: {suspension.reason}</p>
          </div>
          <Button
            disabled={act.busy}
            onClick={async () => {
              if (await act.run(() => unsuspend(principal))) {
                notify.saved("Suspension lifted");
                susp.reload();
              }
            }}
          >
            Unsuspend
          </Button>
        </div>
      )}

      <TableCard
        title="Sessions and devices"
        count={sessions.data?.length}
        description="Where this user is signed in. Revoking a session signs that device out."
        actions={
          <ConfirmButton
            label="Revoke all"
            disabled={!sessions.data?.length}
            consequence="Every device this user is signed in on is signed out. They can sign in again."
            onConfirm={async () => {
              if (await act.run(() => revokeAllSessions(collection, id))) reloadAll();
            }}
          />
        }
      >
        {!!sessions.error && <CardBody><ErrorPanel error={sessions.error} onRetry={sessions.reload} /></CardBody>}
        {sessions.loading && !sessions.data && !sessions.error && <CardBody><SkeletonTable rows={2} columns={5} label="Loading sessions" /></CardBody>}
        {sessions.data?.length === 0 && (
          <CardBody>
            <EmptyState compact title="No active sessions" why="This user is signed out everywhere." />
          </CardBody>
        )}
        {!!sessions.data?.length && (
          <Table framed aria-label="Sessions" className="text-sm">
            <Table.Header className={HEAD_ROW}>
              <Table.Row><Th>Device</Th><Th>IP address</Th><Th>Method</Th><Th>Last seen</Th><Th>Expires</Th><Th><span className="sr-only">Actions</span></Th></Table.Row>
            </Table.Header>
            <Table.Body>
              {sessions.data.map((s) => (
                <Table.Row key={s.id}>
                  <Table.Cell>
                    <div className="flex flex-wrap items-center gap-1.5">
                      <span className="font-medium" title={s.user_agent ?? undefined}>{s.device_name ?? agentLabel(s.user_agent)}</span>
                      {s.current && <Badge tone="info">This session</Badge>}
                      {s.mfa && <Badge tone="ok">MFA</Badge>}
                      {s.impersonator && <Badge tone="warn">Impersonated by {s.impersonator}</Badge>}
                    </div>
                    {s.device_name && s.user_agent && <p className="text-xs text-muted-foreground" title={s.user_agent}>{agentLabel(s.user_agent)}</p>}
                  </Table.Cell>
                  <Table.Cell className="font-mono text-xs">{s.ip ?? "—"}</Table.Cell>
                  <Table.Cell>{methodLabel(s.auth_method)}</Table.Cell>
                  <Table.Cell className="text-muted-foreground"><When value={s.last_seen_at} /></Table.Cell>
                  <Table.Cell className="text-muted-foreground"><When value={s.expires_at} /></Table.Cell>
                  <Table.Cell align="end">
                    <Button
                      tone="danger"
                      aria-label={`Revoke session ${s.id}`}
                      disabled={act.busy}
                      onClick={async () => {
                        if (await act.run(() => revokeSession(s.id))) reloadAll();
                      }}
                    >
                      Revoke
                    </Button>
                  </Table.Cell>
                </Table.Row>
              ))}
            </Table.Body>
          </Table>
        )}
      </TableCard>

      <TableCard title="Sign-in history" count={history.data?.length} description="The latest 50 sign-in events for this account, newest first.">
        {!!history.error && <CardBody><ErrorPanel error={history.error} onRetry={history.reload} /></CardBody>}
        {history.loading && !history.data && !history.error && <CardBody><SkeletonTable rows={3} columns={5} label="Loading history" /></CardBody>}
        {history.data?.length === 0 && (
          <CardBody>
            <EmptyState compact title="No sign-in events" why="Sign-ins, failed attempts and sign-outs of this account will show here." />
          </CardBody>
        )}
        {!!history.data?.length && (
          <Table framed density="compact" aria-label="Sign-in history" className="text-sm">
            <Table.Header className={HEAD_ROW}>
              <Table.Row><Th>When</Th><Th>Event</Th><Th>Outcome</Th><Th>IP address</Th><Th>Client</Th></Table.Row>
            </Table.Header>
            <Table.Body>
              {history.data.map((h) => (
                <Table.Row key={h.id}>
                  <Table.Cell className="text-muted-foreground"><When value={h.at} /></Table.Cell>
                  <Table.Cell>
                    <span className="flex flex-wrap items-center gap-x-2">
                      <span>{eventLabel(h.event)}</span>
                      <code className="font-mono text-xs text-muted-foreground">{h.event}</code>
                    </span>
                  </Table.Cell>
                  <Table.Cell>{h.outcome === "failure" ? <Badge tone="bad" dot>Failure</Badge> : <Badge tone="ok" dot>Success</Badge>}</Table.Cell>
                  <Table.Cell className="font-mono text-xs">{h.ip ?? "—"}</Table.Cell>
                  <Table.Cell className="max-w-56 truncate text-xs text-muted-foreground" title={h.user_agent ?? undefined}>{h.user_agent ? agentLabel(h.user_agent) : "—"}</Table.Cell>
                </Table.Row>
              ))}
            </Table.Body>
          </Table>
        )}
      </TableCard>

      <TableCard title="Account fields" description={<>The record in <code>{collection}</code>. Edit it in the Database studio.</>}>
        <dl className="divide-y divide-border text-sm">
          {Object.entries(user).map(([k, v]) => (
            <div key={k} className="grid grid-cols-[minmax(8rem,12rem)_1fr] gap-4 px-4 py-2">
              <dt className="font-mono text-xs leading-5 text-muted-foreground">{k}</dt>
              <dd className="min-w-0 break-all font-mono text-xs leading-5">
                {v === null || v === undefined || v === "" ? <span className="text-muted-foreground">—</span> : typeof v === "boolean" ? <Chip tone={v ? "ok" : undefined}>{String(v)}</Chip> : typeof v === "object" ? JSON.stringify(v) : String(v)}
              </dd>
            </div>
          ))}
        </dl>
      </TableCard>

      {!suspension && (
        <section aria-label="Suspend account" className="rounded-xl border border-destructive/30 bg-card">
          <div className="border-b border-destructive/20 px-4 py-3">
            <h2 className="text-sm font-semibold">Suspend account</h2>
            <p className="mt-0.5 text-xs text-muted-foreground">Blocks this account: its requests are refused until you lift the suspension. The reason is kept in the audit log.</p>
          </div>
          <form
            className="flex flex-wrap items-center gap-2 px-4 py-3"
            onSubmit={async (e) => {
              e.preventDefault();
              if (await act.run(() => suspend(principal, reason.trim()))) {
                setReason("");
                notify.saved("Suspension");
                susp.reload();
              }
            }}
          >
            <Input size="sm" aria-label="Suspension reason" className="w-full max-w-sm" placeholder="Reason, for example: abuse report 42" value={reason} onChange={(e) => setReason(e.target.value)} />
            <Button type="submit" tone="danger" disabled={act.busy || !reason.trim()}>
              Suspend
            </Button>
          </form>
        </section>
      )}
      {showImp && <ImpersonateDialog principal={principal} email={email} onClose={() => setShowImp(false)} />}
    </div>
  );
}

const DURATIONS = [5, 15, 30, 60].map((m) => ({ value: m, label: `${m} minutes` }));

function ImpersonateDialog({ principal, email, onClose }: { principal: string; email: string; onClose: () => void }) {
  const session = useSession();
  const [reason, setReason] = useState("");
  const [minutes, setMinutes] = useState(15);
  const act = useAction();
  const formId = `imp${useId().replace(/:/g, "")}`;
  return (
    <Modal
      title="Impersonate user"
      description={<>You will act as <strong className="font-medium text-foreground">{email}</strong> and see what they see. Every action is logged with your administrator id.</>}
      onClose={onClose}
      footer={
        <>
          <Button onClick={onClose}>Cancel</Button>
          <Button type="submit" form={formId} tone="primary" disabled={act.busy || reason.trim().length < 3}>
            {act.busy ? "Starting…" : "Start impersonation"}
          </Button>
        </>
      }
    >
      <form
        id={formId}
        className="space-y-4 text-sm"
        onSubmit={async (e) => {
          e.preventDefault();
          if (reason.trim().length < 3) return;
          if (await act.run(() => session.startImpersonation(principal, reason.trim(), minutes))) onClose();
        }}
      >
        <FormField label="Reason" term="impersonation" help="Required, at least 3 characters. Saved in the audit log, for example a support ticket number.">
          {(p) => <Textarea {...p} className="w-full" value={reason} onChange={(e) => setReason(e.target.value)} />}
        </FormField>
        <FormField label="Duration" help="The impersonation ends by itself after this long.">
          {(p) => <Sel {...p} size="md" className="w-40" value={minutes} onChange={setMinutes} options={DURATIONS} />}
        </FormField>
        {act.errorView}
      </form>
    </Modal>
  );
}
