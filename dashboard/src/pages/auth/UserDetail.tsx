import { Textarea } from "panelui-web/textarea";
import { useState } from "react";
import { useParams } from "react-router-dom";
import {
  getUserDetail, listSuspensions, revokeAllSessions, revokeSession, suspend, unsuspend, userLoginHistory, userSessions,
} from "../../api";
import { useSession } from "../../auth/session";
import { Button, ConfirmButton, ErrorView, Loading, Panel } from "../../components/ui";
import { useLoad } from "../../hooks";
import { Badge, Modal, THEAD, Th, fmtTime, useAction } from "./common";
import { Table } from "panelui-web/table";
import { Input } from "panelui-web/input";

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

  const reloadAll = () => {
    detail.reload();
    sessions.reload();
  };

  return (
    <div className="space-y-4">
      <ErrorView error={detail.error} />
      {detail.loading && !detail.error && <Loading what="Loading user" />}
      {detail.data && (
        <>
          <div className="flex flex-wrap items-center justify-between gap-2">
            <h2 className="text-xl font-semibold">{String(detail.data.user.email ?? id)}</h2>
            <div className="flex items-center gap-2">
              {suspension ? <Badge tone="bad">Suspended</Badge> : <Badge tone="ok">Active</Badge>}
              <Button onClick={() => setShowImp(true)}>Impersonate</Button>
            </div>
          </div>
          {act.errorView}
          <Panel title="Account">
            <dl className="grid grid-cols-[10rem_1fr] gap-y-1 text-sm">
              <dt className="text-muted-foreground">MFA</dt>
              <dd>{detail.data.mfa_enabled ? <Badge tone="ok">enabled</Badge> : <Badge>not enabled</Badge>}</dd>
              <dt className="text-muted-foreground">Active sessions</dt>
              <dd>{detail.data.active_sessions}</dd>
              <dt className="text-muted-foreground">Last sign-in</dt>
              <dd>{fmtTime(detail.data.last_login_at)}</dd>
              {Object.entries(detail.data.user).map(([k, v]) => (
                <div key={k} className="contents">
                  <dt className="text-muted-foreground">{k}</dt>
                  <dd className="break-all font-mono text-xs">{typeof v === "object" ? JSON.stringify(v) : String(v)}</dd>
                </div>
              ))}
            </dl>
          </Panel>
          <Panel title="Suspension">
            {suspension ? (
              <div className="flex flex-wrap items-center gap-3 text-sm">
                <span>
                  Suspended: <em>{suspension.reason}</em>
                  {suspension.until ? ` until ${fmtTime(suspension.until)}` : ""}
                </span>
                <Button
                  disabled={act.busy}
                  onClick={async () => {
                    if (await act.run(() => unsuspend(principal))) susp.reload();
                  }}
                >
                  Unsuspend
                </Button>
              </div>
            ) : (
              <form
                className="flex flex-wrap items-center gap-2"
                onSubmit={async (e) => {
                  e.preventDefault();
                  if (await act.run(() => suspend(principal, reason.trim()))) {
                    setReason("");
                    susp.reload();
                  }
                }}
              >
                <Input size="sm" aria-label="Suspension reason" className="w-72" placeholder="Reason" value={reason} onChange={(e) => setReason(e.target.value)} />
                <Button type="submit" tone="danger" disabled={act.busy || !reason.trim()}>
                  Suspend
                </Button>
              </form>
            )}
          </Panel>
          <Panel
            title="Sessions and devices"
            actions={
              <ConfirmButton
                label="Revoke all"
                disabled={!sessions.data?.length}
                onConfirm={async () => {
                  if (await act.run(() => revokeAllSessions(collection, id))) reloadAll();
                }}
              />
            }
          >
            <ErrorView error={sessions.error} />
            {sessions.loading && !sessions.error && <Loading what="Loading sessions" />}
            {sessions.data?.length === 0 && <p className="text-sm text-muted-foreground">No active sessions.</p>}
            {!!sessions.data?.length && (
              <Table className="w-full text-sm">
                <Table.Header className={THEAD}>
                  <Table.Row><Th>Device</Th><Th>IP</Th><Th>Method</Th><Th>Last seen</Th><Th>Expires</Th><Th> </Th></Table.Row>
                </Table.Header>
                <Table.Body>
                  {sessions.data.map((s) => (
                    <Table.Row key={s.id} className="border-t border-border">
                      <Table.Cell className="py-2 pr-3">
                        {s.device_name ?? s.user_agent ?? "unknown"} {s.mfa && <Badge tone="ok">MFA</Badge>}{" "}
                        {s.impersonator && <Badge tone="warn">impersonated by {s.impersonator}</Badge>}
                      </Table.Cell>
                      <Table.Cell className="pr-3 font-mono text-xs">{s.ip ?? "—"}</Table.Cell>
                      <Table.Cell className="pr-3">{s.auth_method}</Table.Cell>
                      <Table.Cell className="pr-3">{fmtTime(s.last_seen_at)}</Table.Cell>
                      <Table.Cell className="pr-3">{fmtTime(s.expires_at)}</Table.Cell>
                      <Table.Cell>
                        <Button
                          tone="danger"
                          aria-label={`Revoke session ${s.id}`}
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
          </Panel>
          <Panel title="Login history">
            <ErrorView error={history.error} />
            {history.loading && !history.error && <Loading what="Loading history" />}
            {history.data?.length === 0 && <p className="text-sm text-muted-foreground">No sign-in events.</p>}
            {!!history.data?.length && (
              <Table className="w-full text-sm">
                <Table.Header className={THEAD}>
                  <Table.Row><Th>When</Th><Th>Event</Th><Th>Outcome</Th><Th>IP</Th><Th>Agent</Th></Table.Row>
                </Table.Header>
                <Table.Body>
                  {history.data.map((h) => (
                    <Table.Row key={h.id} className="border-t border-border">
                      <Table.Cell className="py-1.5 pr-3">{fmtTime(h.at)}</Table.Cell>
                      <Table.Cell className="pr-3">{h.event}</Table.Cell>
                      <Table.Cell className="pr-3">{h.outcome === "failure" ? <Badge tone="bad">failure</Badge> : <Badge tone="ok">success</Badge>}</Table.Cell>
                      <Table.Cell className="pr-3 font-mono text-xs">{h.ip ?? "—"}</Table.Cell>
                      <Table.Cell className="pr-3 truncate text-xs text-muted-foreground">{h.user_agent ?? ""}</Table.Cell>
                    </Table.Row>
                  ))}
                </Table.Body>
              </Table>
            )}
          </Panel>
        </>
      )}
      {showImp && <ImpersonateDialog principal={principal} onClose={() => setShowImp(false)} />}
    </div>
  );
}

function ImpersonateDialog({ principal, onClose }: { principal: string; onClose: () => void }) {
  const session = useSession();
  const [reason, setReason] = useState("");
  const [minutes, setMinutes] = useState(15);
  const act = useAction();
  return (
    <Modal title="Impersonate user" onClose={onClose}>
      <form
        className="space-y-3 text-sm"
        onSubmit={async (e) => {
          e.preventDefault();
          if (await act.run(() => session.startImpersonation(principal, reason.trim(), minutes))) onClose();
        }}
      >
        <p className="text-muted-foreground">
          You will act as <span className="font-mono">{principal}</span>. Every action is audited with your admin id.
        </p>
        <label className="block">
          Reason (required)
          <Textarea aria-label="Reason" className="mt-1 w-full" value={reason} onChange={(e) => setReason(e.target.value)} />
        </label>
        <label className="block">
          Minutes (1–60)
          <Input size="sm" aria-label="Minutes" type="number" min={1} max={60} className="mt-1 w-24" value={minutes} onChange={(e) => setMinutes(Number(e.target.value))} />
        </label>
        {act.errorView}
        <div className="flex justify-end gap-2">
          <Button onClick={onClose}>Cancel</Button>
          <Button type="submit" tone="primary" disabled={act.busy || reason.trim().length < 3}>
            Start impersonation
          </Button>
        </div>
      </form>
    </Modal>
  );
}
