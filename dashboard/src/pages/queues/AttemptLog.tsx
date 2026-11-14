import { useState } from "react";
import { Input } from "panelui-web/input";
import { getPushAttempt, listPushAttempts, retryQueueNow, testPushEndpoint } from "../../api";
import type { PushAttempt, PushOutcome } from "../../api";
import { Button, ErrorView } from "../../components/ui";
import { DetailSheet } from "../../components/CreateSheet";
import { EmptyState } from "../../components/EmptyState";
import { HelpTip } from "../../components/HelpTip";
import { Pager } from "../../components/Pager";
import { Badge } from "../../components/StatusBadge";
import { SkeletonTable } from "../../components/Skeleton";
import { CardBody, HEAD_ROW, TableCard } from "../../components/TableCard";
import { Sel } from "../../components/controls";
import { useCursorList } from "../../hooks";
import { Table } from "panelui-web/table";
import { Code, Facts, NUM, When } from "../jobs/ops";
import { Filter } from "../billing/shared";
import { done, maskHeader } from "./webhook";

const OUTCOMES = [{ value: "", label: "All outcomes" }, { value: "success", label: "Success" }, { value: "failure", label: "Failure" }];
const KINDS = [{ value: "", label: "Deliveries and tests" }, { value: "false", label: "Deliveries only" }, { value: "true", label: "Test sends only" }];

/** Header rows, with the signature (and any credential) masked. */
function Headers({ label, headers }: { label: string; headers: Record<string, string> }) {
  const rows = Object.entries(headers);
  if (rows.length === 0) return <p className="text-xs text-muted-foreground">None recorded.</p>;
  return (
    <table aria-label={label} className="w-full text-left text-xs">
      <tbody>
        {rows.map(([k, v]) => (
          <tr key={k} className="border-t border-border first:border-t-0">
            <th scope="row" className="w-2/5 py-1 pr-3 align-top font-mono font-normal text-muted-foreground">{k}</th>
            <td className="break-all py-1 font-mono">{maskHeader(k, v)}</td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}

/** One attempt in a drawer: status, latency, error class, request headers (signature masked), response headers and the cut body. */
function AttemptDrawer({ attempt, onClose, onRetried }: { attempt: PushAttempt | undefined; onClose: () => void; onRetried: () => void }) {
  const [error, setError] = useState<unknown>();
  const [busy, setBusy] = useState(false);
  const a = attempt;
  async function retry() {
    if (!a) return;
    setBusy(true);
    setError(undefined);
    try {
      const r = await retryQueueNow(a.subscription, [a.message_id]);
      done(r.count > 0 ? "Retry started" : "Nothing to retry", r.count > 0 ? undefined : "That message is not waiting for a retry backoff.");
      onRetried();
    } catch (e) {
      setError(e);
    } finally {
      setBusy(false);
    }
  }
  return (
    <DetailSheet
      open={!!a}
      onOpenChange={(v) => { if (!v) { setError(undefined); onClose(); } }}
      title={a ? `Attempt ${a.id}` : "Attempt"}
      description={a ? <span>{a.test ? "Test send" : <>Message <span className="font-mono">{a.message_id}</span>, delivery attempt {a.attempt}</>}</span> : undefined}
      footer={a && !a.test && a.outcome === "failure" ? <Button disabled={busy} onClick={() => void retry()}>Retry now</Button> : undefined}
    >
      {a && (
        <>
          <Facts columns={2} framed={false} label="Attempt facts" items={[
            { label: "Outcome", value: <Badge status={a.outcome} /> },
            { label: "HTTP status", value: a.status ?? "no answer", mono: true },
            { label: "Latency", value: `${a.latency_ms} ms`, mono: true },
            { label: "Error class", value: a.error_class ?? "none", mono: true },
            { label: "When", value: <When iso={a.at} /> },
            { label: a.kind === "webhook" ? "Webhook" : "Function", value: a.endpoint, mono: true },
          ]} />
          {a.error && <p role="alert" className="rounded-lg border border-destructive/40 bg-destructive/10 p-3 text-sm text-destructive">{a.error}</p>}
          <section aria-label="Request headers" className="space-y-1">
            <h3 className="text-sm font-semibold">Request headers</h3>
            <p className="text-xs text-muted-foreground">What ForgeBase sent. The signature is masked.</p>
            <Headers label="Request headers table" headers={a.request_headers} />
          </section>
          <section aria-label="Response headers" className="space-y-1">
            <h3 className="text-sm font-semibold">Response headers</h3>
            <Headers label="Response headers table" headers={a.response_headers} />
          </section>
          <section aria-label="Response body" className="space-y-1">
            <h3 className="text-sm font-semibold">Response body</h3>
            {a.response_body ? <Code label="Response body text">{a.response_body}</Code> : <p className="text-xs text-muted-foreground">The endpoint sent no body.</p>}
            {a.response_truncated && <p role="note" className="text-xs text-warning">Truncated: the first {new Blob([a.response_body ?? ""]).size.toLocaleString()} of {(a.response_bytes ?? 0).toLocaleString()} bytes are kept.</p>}
          </section>
          <ErrorView error={error} />
        </>
      )}
    </DetailSheet>
  );
}

/** The attempt log of a push subscription: filters, a page at a time, a drawer per attempt, and "Send test event". */
export default function AttemptLog({ sub, onChanged }: { sub: string; onChanged: () => void }) {
  const [outcome, setOutcome] = useState<PushOutcome | "">("");
  const [messageId, setMessageId] = useState("");
  const [kind, setKind] = useState<"" | "true" | "false">("");
  const [since, setSince] = useState("");
  const [open, setOpen] = useState<PushAttempt>();
  const [testing, setTesting] = useState(false);
  const [error, setError] = useState<unknown>();
  const sinceIso = since && !Number.isNaN(new Date(since).getTime()) ? new Date(since).toISOString() : undefined;
  const filter = { outcome, message_id: messageId.trim() || undefined, test: kind === "" ? ("" as const) : kind === "true", since: sinceIso };
  const list = useCursorList(
    (cursor, signal) => listPushAttempts(sub, { ...filter, before: cursor ? Number(cursor) : undefined }, signal).then((p) => ({ items: p.items, next: p.next_before == null ? null : String(p.next_before) })),
    `attempts:${sub}:${outcome}:${messageId.trim()}:${kind}:${sinceIso ?? ""}`,
  );

  async function sendTest() {
    setError(undefined);
    setTesting(true);
    try {
      const a = await testPushEndpoint(sub);
      setOpen(a);
      done(a.outcome === "success" ? `Test event accepted (${a.status ?? "ok"})` : "Test event failed", a.outcome === "success" ? `${a.latency_ms} ms` : (a.error ?? undefined));
      list.reload();
      onChanged();
    } catch (e) {
      setError(e);
    } finally {
      setTesting(false);
    }
  }
  async function openRow(a: PushAttempt) {
    setOpen(a);
    try { setOpen(await getPushAttempt(sub, a.id)); } catch { /* the row already has everything the log shows */ }
  }

  const toolbar = (
    <>
      <Filter label="Outcome">{(id) => <Sel id={id} value={outcome} onChange={(v) => setOutcome(v as PushOutcome | "")} options={OUTCOMES} className="w-40" />}</Filter>
      <Filter label="Message id">{(id) => <Input id={id} size="sm" className="w-56 font-mono" value={messageId} onChange={(e) => setMessageId(e.target.value)} placeholder="Any message" />}</Filter>
      <Filter label="Kind">{(id) => <Sel id={id} value={kind} onChange={(v) => setKind(v as "" | "true" | "false")} options={KINDS} className="w-48" />}</Filter>
      <Filter label="Since">{(id) => <Input id={id} size="sm" type="datetime-local" className="w-52" value={since} onChange={(e) => setSince(e.target.value)} />}</Filter>
    </>
  );
  const filtered = !!outcome || !!messageId.trim() || kind !== "" || !!sinceIso;
  return (
    <TableCard
      title={<span className="inline-flex items-center gap-1">Attempt log <HelpTip term="push_attempt" /></span>}
      count={list.items.length ? `${list.items.length}${list.hasMore ? "+" : ""}` : undefined}
      description="Every call to the endpoint, newest first: deliveries and test sends. The newest 1000 are kept for 7 days."
      actions={<><Button onClick={list.reload}>Refresh</Button><Button tone="primary" disabled={testing} onClick={() => void sendTest()}>{testing ? "Sending…" : "Send test event"}</Button></>}
      toolbar={toolbar}
      footer={list.items.length > 0 ? <Pager mode="more" noun="attempts" shown={list.items.length} hasMore={list.hasMore} loading={list.loadingMore} onMore={list.loadMore} label="Attempt log pages" /> : undefined}
    >
      <CardBody className="space-y-3 empty:hidden">
        <ErrorView error={error ?? list.error} onRetry={list.reload} />
        {list.loading && list.items.length === 0 && !list.error && <SkeletonTable rows={3} columns={7} label="Loading attempts" />}
        {!list.loading && list.items.length === 0 && !list.error && (
          filtered
            ? <EmptyState compact title="No attempts match" why="Change or clear the filters to see more." />
            : <EmptyState compact title="No attempts yet" why="Each call to the endpoint is logged here. Send a test event to check the endpoint without publishing a real message." />
        )}
      </CardBody>
      {list.items.length > 0 && (
        <Table framed density="compact" aria-label="Attempts">
          <Table.Header className={HEAD_ROW}>
            <Table.Row><Table.Head className="py-1">When</Table.Head><Table.Head>Outcome</Table.Head><Table.Head className={NUM}>Status</Table.Head><Table.Head className={NUM}>Latency</Table.Head><Table.Head>Error class</Table.Head><Table.Head>Message</Table.Head><Table.Head className={NUM}>Attempt</Table.Head><Table.Head><span className="sr-only">Actions</span></Table.Head></Table.Row>
          </Table.Header>
          <Table.Body>
            {list.items.map((a) => (
              <Table.Row key={a.id}>
                <Table.Cell className="py-2"><When iso={a.at} /></Table.Cell>
                <Table.Cell><span className="inline-flex items-center gap-1.5"><Badge status={a.outcome} />{a.test && <Badge>test</Badge>}</span></Table.Cell>
                <Table.Cell className={`${NUM} font-mono`}>{a.status ?? "—"}</Table.Cell>
                <Table.Cell className={NUM}>{a.latency_ms} ms</Table.Cell>
                <Table.Cell className="font-mono text-xs">{a.error_class ?? <span className="text-muted-foreground">—</span>}</Table.Cell>
                <Table.Cell className="max-w-48 truncate font-mono text-xs" title={a.message_id}>{a.message_id}</Table.Cell>
                <Table.Cell className={NUM}>{a.test ? "—" : a.attempt}</Table.Cell>
                <Table.Cell className="text-right"><Button aria-label={`Show attempt ${a.id}`} onClick={() => void openRow(a)}>Details</Button></Table.Cell>
              </Table.Row>
            ))}
          </Table.Body>
        </Table>
      )}
      <AttemptDrawer attempt={open} onClose={() => setOpen(undefined)} onRetried={() => { list.reload(); onChanged(); }} />
    </TableCard>
  );
}
