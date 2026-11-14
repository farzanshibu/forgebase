import { useState } from "react";
import { Table } from "panelui-web/table";
import { deleteNotifier, listNotifierDeliveries, listNotifiers, testNotifier } from "../../api";
import type { Notifier, NotifierDelivery, NotifierDeliveryStatus } from "../../api";
import { ConfirmDialog } from "../../components/ConfirmDialog";
import { EmptyState } from "../../components/EmptyState";
import { Sel } from "../../components/controls";
import { SkeletonTable } from "../../components/Skeleton";
import { Badge } from "../../components/StatusBadge";
import { CardBody, HEAD_ROW, NUM, TableCard } from "../../components/TableCard";
import { Button, CopyButton, ErrorView } from "../../components/ui";
import { useCreateParam, useLoad } from "../../hooks";
import { notify } from "../../toaster";
import { Callout, When } from "../jobs/ops";
import NotifierSheet from "./NotifierSheet";
import { SeverityBadge } from "./shared";

/** Where a notifier sends to, without a secret: the URL (Slack: its host), recipients or topic. */
function target(n: Notifier): string {
  if (n.kind === "email") return n.to.join(", ");
  if (n.kind === "queue") return n.topic ?? "";
  if (n.kind === "pagerduty") return n.url ?? "events.pagerduty.com";
  return n.url ?? "";
}

function Deliveries({ notifiers, refreshKey }: { notifiers: Notifier[]; refreshKey: number }) {
  const [status, setStatus] = useState<"" | NotifierDeliveryStatus>("");
  const [notifier, setNotifier] = useState("");
  const list = useLoad((s) => listNotifierDeliveries({ status: status || undefined, notifier: notifier || undefined, limit: 100 }, s), `deliveries:${status}:${notifier}:${refreshKey}`);
  const items = list.data ?? [];
  const nameOf = (id: string) => notifiers.find((n) => n.id === id)?.name ?? id;
  return (
    <TableCard
      title="Deliveries"
      count={list.data?.length}
      description="Every notification, newest first. A failed attempt is retried (30 seconds, doubling up to an hour, eight attempts), then marked failed."
      actions={
        <span className="flex gap-2">
          <Sel aria-label="Notifier" value={notifier} onChange={setNotifier} options={[{ value: "", label: "Any notifier" }, ...notifiers.map((n) => ({ value: n.id, label: n.name }))]} />
          <Sel aria-label="Delivery status" value={status} onChange={setStatus} options={[{ value: "", label: "Any status" }, "pending", "sent", "failed"]} />
          <Button onClick={list.reload}>Refresh</Button>
        </span>
      }
    >
      <CardBody><ErrorView error={list.error} onRetry={list.reload} /></CardBody>
      {list.loading && !list.data && <CardBody><SkeletonTable rows={3} columns={6} label="Loading deliveries" /></CardBody>}
      {list.data && items.length === 0 && <CardBody><EmptyState compact title="No deliveries" why="A notification is recorded here when a notifier is told about an alert, or when you send a test." /></CardBody>}
      {items.length > 0 && (
        <Table framed density="compact" aria-label="Deliveries">
          <Table.Header className={HEAD_ROW}>
            <Table.Row><Table.Head>Notifier</Table.Head><Table.Head>Event</Table.Head><Table.Head>Status</Table.Head><Table.Head className={NUM}>Attempts</Table.Head><Table.Head>Last result</Table.Head><Table.Head>Created</Table.Head></Table.Row>
          </Table.Header>
          <Table.Body>
            {items.map((d) => (
              <Table.Row key={d.id}>
                <Table.Cell className="font-medium">{nameOf(d.notifier_id)}</Table.Cell>
                <Table.Cell>{d.event}</Table.Cell>
                <Table.Cell><Badge status={d.status}>{d.status}</Badge></Table.Cell>
                <Table.Cell className={NUM}>{d.attempts}</Table.Cell>
                <Table.Cell className="max-w-72 truncate text-muted-foreground" title={d.last_error ?? undefined}>
                  {d.last_status ? `HTTP ${d.last_status}` : ""}{d.last_error ? ` ${d.last_error}` : ""}{!d.last_status && !d.last_error ? (d.next_attempt_at ? <>next try <When iso={d.next_attempt_at} /></> : "—") : ""}
                </Table.Cell>
                <Table.Cell><When iso={d.created_at} /></Table.Cell>
              </Table.Row>
            ))}
          </Table.Body>
        </Table>
      )}
    </TableCard>
  );
}

/** Notifiers (secrets never listed), a test button, and the delivery list. */
export default function Notifiers() {
  const [creating, setCreating] = useCreateParam();
  const list = useLoad(listNotifiers, "alert-notifiers");
  const [editing, setEditing] = useState<Notifier>();
  const [secret, setSecret] = useState<Notifier>();
  const [testing, setTesting] = useState<string>();
  const [tested, setTested] = useState<{ notifier: Notifier; delivery: NotifierDelivery }>();
  const [error, setError] = useState<unknown>();
  const [deliveriesKey, setDeliveriesKey] = useState(0);

  async function test(n: Notifier) {
    setTesting(n.id);
    setError(undefined);
    setTested(undefined);
    try {
      setTested({ notifier: n, delivery: await testNotifier(n.id) });
      setDeliveriesKey((k) => k + 1);
      list.reload();
    } catch (e) {
      setError(e);
    } finally {
      setTesting(undefined);
    }
  }

  const saved = (n: Notifier) => {
    if (n.signing_secret) setSecret(n);
    list.reload();
    setDeliveriesKey((k) => k + 1);
  };
  const items = list.data ?? [];
  return (
    <div className="space-y-4">
      {secret?.signing_secret && (
        <Callout tone="warn" role="status" title={`Signing secret of ${secret.name}`}>
          <p>Copy it now: it is shown once and cannot be read again. Receivers verify the <code>X-Forgebase-Signature</code> header with it.</p>
          <p className="mt-2 flex flex-wrap items-center gap-2"><code aria-label="Signing secret" className="rounded bg-muted px-2 py-1 font-mono text-xs">{secret.signing_secret}</code><CopyButton text={secret.signing_secret} label="Copy signing secret" /><Button onClick={() => setSecret(undefined)}>I have copied it</Button></p>
        </Callout>
      )}
      <ErrorView error={error} />
      {tested && (
        <Callout tone={tested.delivery.status === "sent" ? "ok" : "bad"} role="status" title={tested.delivery.status === "sent" ? `Test sent to ${tested.notifier.name}` : `Test to ${tested.notifier.name} ${tested.delivery.status === "failed" ? "failed" : "is queued"}`}>
          {tested.delivery.status === "sent" ? `Delivered${tested.delivery.last_status ? ` (HTTP ${tested.delivery.last_status})` : ""}.` : tested.delivery.last_error ?? "It will be retried; see Deliveries below."}
        </Callout>
      )}
      <TableCard title="Notifiers" count={list.data?.length} description="Webhook, Slack, email, PagerDuty or a queue topic. Secrets are stored sealed and never listed." actions={<Button onClick={list.reload}>Refresh</Button>}>
        <CardBody><ErrorView error={list.error} onRetry={list.reload} /></CardBody>
        {list.loading && !list.data && <CardBody><SkeletonTable rows={3} columns={6} label="Loading notifiers" /></CardBody>}
        {list.data && items.length === 0 && <CardBody><EmptyState compact title="No notifiers" why="Without a notifier, alerts only show in this dashboard. Add one to be told when something fires." action={<Button tone="primary" onClick={() => setCreating(true)}>New notifier</Button>} /></CardBody>}
        {items.length > 0 && (
          <Table framed density="compact" aria-label="Notifiers">
            <Table.Header className={HEAD_ROW}>
              <Table.Row><Table.Head>Notifier</Table.Head><Table.Head>Sends to</Table.Head><Table.Head>From</Table.Head><Table.Head>Last delivery</Table.Head><Table.Head><span className="sr-only">Actions</span></Table.Head></Table.Row>
            </Table.Header>
            <Table.Body>
              {items.map((n) => (
                <Table.Row key={n.id}>
                  <Table.Cell className="py-2"><span className="flex items-center gap-2 font-medium">{n.name}<Badge>{n.kind}</Badge>{!n.enabled && <Badge tone="neutral">disabled</Badge>}</span></Table.Cell>
                  <Table.Cell className="max-w-64 truncate font-mono text-xs" title={target(n)}>{target(n)}</Table.Cell>
                  <Table.Cell><SeverityBadge severity={n.min_severity} /></Table.Cell>
                  <Table.Cell className="max-w-72 text-xs">
                    {n.last_error ? <span className="text-destructive" title={n.last_error}>Failed <When iso={n.last_failure_at} />: {n.last_error}</span> : n.last_success_at ? <span className="text-muted-foreground">Sent <When iso={n.last_success_at} /></span> : <span className="text-muted-foreground">Nothing sent yet</span>}
                  </Table.Cell>
                  <Table.Cell className="text-right">
                    <span className="inline-flex gap-2">
                      <Button aria-label={`Send a test to ${n.name}`} disabled={testing === n.id} onClick={() => void test(n)}>{testing === n.id ? "Sending…" : "Test"}</Button>
                      <Button aria-label={`Edit ${n.name}`} onClick={() => setEditing(n)}>Edit</Button>
                      <ConfirmDialog
                        trigger={<Button tone="danger" aria-label={`Delete ${n.name}`}>Delete</Button>}
                        title={`Delete notifier ${n.name}?`}
                        consequence="Its delivery history is deleted too, and nothing more is sent to it."
                        confirmLabel="Delete notifier"
                        onConfirm={async () => { await deleteNotifier(n.id); notify.deleted(`Notifier ${n.name}`); list.reload(); }}
                      />
                    </span>
                  </Table.Cell>
                </Table.Row>
              ))}
            </Table.Body>
          </Table>
        )}
      </TableCard>
      <Deliveries notifiers={items} refreshKey={deliveriesKey} />
      {creating && <NotifierSheet open onOpenChange={setCreating} onSaved={saved} />}
      {editing && <NotifierSheet key={editing.id} open notifier={editing} onOpenChange={(o) => !o && setEditing(undefined)} onSaved={saved} />}
    </div>
  );
}
