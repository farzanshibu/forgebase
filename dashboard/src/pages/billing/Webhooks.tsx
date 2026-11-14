import { useState } from "react";
import { Link } from "react-router-dom";
import { Input } from "panelui-web/input";
import { Table } from "panelui-web/table";
import { getWebhookDelivery, listWebhookDeliveries, replayWebhookDelivery, reverifyWebhookDelivery, WEBHOOK_OUTCOMES } from "../../api";
import type { WebhookDelivery, WebhookDeliveryDetail, WebhookDeliveryResult } from "../../api";
import { Sel } from "../../components/controls";
import { ConfirmDialog } from "../../components/ConfirmDialog";
import { DetailSheet } from "../../components/CreateSheet";
import { EmptyState } from "../../components/EmptyState";
import { HelpTip } from "../../components/HelpTip";
import { Pager } from "../../components/Pager";
import { SkeletonLines, SkeletonTable } from "../../components/Skeleton";
import { Badge } from "../../components/StatusBadge";
import { Th } from "../../components/TableCard";
import { When } from "../../components/Time";
import { Button, CopyButton, ErrorView } from "../../components/ui";
import { useLoad } from "../../hooks";
import { toaster } from "../../toaster";
import { Callout, Code, Facts } from "../jobs/ops";
import { BillingError, Filter, None, TableCard } from "./shared";
import { OUTCOME_HELP, OUTCOME_TONE, OUTCOME_WORDS, WEBHOOK_PROVIDERS, bodyView, isRefused, resultWords, touchedObjects } from "./webhookUtil";

const PER_PAGE = 50;
const ANY = "";

function Outcome({ outcome }: { outcome: string }) {
  return <Badge tone={OUTCOME_TONE[outcome] ?? "neutral"} dot>{OUTCOME_WORDS[outcome] ?? outcome}</Badge>;
}

const ms = (us: number | null | undefined) => (us == null ? "—" : us < 1000 ? `${us} µs` : `${+(us / 1000).toFixed(1)} ms`);

/** The headers the server kept (an allowlist: never credentials or cookies). */
function HeaderTable({ headers }: { headers: Record<string, unknown> }) {
  const rows = Object.entries(headers);
  if (rows.length === 0) return <p className="text-xs text-muted-foreground">No headers were kept.</p>;
  return (
    <table aria-label="Stored headers" className="w-full text-left text-xs">
      <tbody>
        {rows.map(([k, v]) => (
          <tr key={k} className="border-t border-border first:border-t-0">
            <th scope="row" className="w-2/5 py-1 pr-3 align-top font-mono font-normal text-muted-foreground">{k}</th>
            <td className="break-all py-1 font-mono">{typeof v === "string" ? v : JSON.stringify(v)}</td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}

function Body({ d }: { d: WebhookDeliveryDetail }) {
  const v = bodyView(d);
  if (v.kind === "none") return <Callout tone="warn" role="note">The body was not kept: unverified deliveries are stored within a budget, and the raw body is cut at 256 KiB. Replay and Re-verify need it.</Callout>;
  return (
    <div className="space-y-2">
      {v.kind === "base64" && <p role="note" className="text-xs text-muted-foreground">Binary body, shown as base64 ({v.bytes?.toLocaleString()} bytes).</p>}
      <Code label={v.kind === "base64" ? "Body (base64)" : "Body"}>{v.text}</Code>
      <div className="flex flex-wrap items-center gap-3">
        <CopyButton text={v.text} label="Copy body" />
        {d.body_truncated && <span role="note" className="text-xs text-warning">Cut at the size limit: {d.body_size.toLocaleString()} bytes were received.</span>}
      </div>
    </div>
  );
}

/** One delivery in a drawer, with Replay (a verified one) or Re-verify (a refused one). The result is drawn in words, a 409 as an explained error. */
function Drawer({ id, onClose, onChanged }: { id: string | undefined; onClose: () => void; onChanged: () => void }) {
  const detail = useLoad((s) => (id ? getWebhookDelivery(id, s) : Promise.resolve(undefined)), `wh:${id ?? ""}`);
  const [result, setResult] = useState<{ id: string; r: WebhookDeliveryResult }>();
  const [error, setError] = useState<unknown>();
  const [busy, setBusy] = useState(false);
  const d = id ? detail.data : undefined;
  const shown = result && result.id === id ? result : undefined;

  async function act(kind: "replay" | "reverify") {
    if (!d) return;
    setError(undefined);
    setBusy(true);
    try {
      const r = await (kind === "replay" ? replayWebhookDelivery : reverifyWebhookDelivery)(d.id);
      const w = resultWords(r);
      setResult({ id: d.id, r });
      toaster.add({ title: `${kind === "replay" ? "Replay" : "Re-verify"}: ${w.title}`, description: w.text, type: w.tone === "bad" ? "destructive" : w.tone === "warn" ? "warning" : "success", timeout: 6000 });
      detail.reload();
      onChanged();
    } catch (e) {
      setError(e);
    } finally {
      setBusy(false);
    }
  }
  const refused = d ? isRefused(d) : false;
  const touched = d ? touchedObjects(d.links) : [];
  const words = shown ? resultWords(shown.r) : undefined;
  return (
    <DetailSheet
      open={!!id}
      onOpenChange={(v) => { if (!v) { setError(undefined); setResult(undefined); onClose(); } }}
      title={id ? `Delivery ${id}` : "Delivery"}
      description={d ? <span className="capitalize">{d.provider}{d.account ? ` · ${d.account}` : ""}</span> : undefined}
      footer={d && (refused ? (
        <ConfirmDialog
          trigger={<Button tone="primary" disabled={busy}>Re-verify</Button>}
          tone="warning"
          title="Re-verify this delivery?"
          consequence="The stored body is checked again with the webhook secrets as they are now. If it verifies, it is processed once, exactly as if the provider had sent it now."
          confirmLabel="Re-verify"
          onConfirm={() => act("reverify")}
        />
      ) : (
        <ConfirmDialog
          trigger={<Button tone="primary" disabled={busy}>Replay</Button>}
          tone="warning"
          title="Replay this delivery?"
          consequence="The stored body is verified again and processed through the normal path. An event that was handled before changes nothing."
          confirmLabel="Replay"
          onConfirm={() => act("replay")}
        />
      ))}
    >
      <ErrorView error={error ?? detail.error} />
      {id && detail.loading && !d && !detail.error && <SkeletonLines lines={5} label="Loading delivery" />}
      {words && <Callout tone={words.tone} title={words.title} role="status">{words.text}</Callout>}
      {d && (
        <>
          {refused && <Callout tone="warn" title={`Refused: ${OUTCOME_WORDS[d.verification] ?? d.verification}`}>{OUTCOME_HELP[d.verification] ?? d.reason ?? "The server refused this delivery."}{d.reason ? <span className="block text-xs text-muted-foreground">Reason: {d.reason}</span> : null}</Callout>}
          <Facts columns={2} framed={false} label="Delivery facts" items={[
            { label: "Outcome", value: <Outcome outcome={d.outcome} /> },
            { label: "Received", value: <When iso={d.received_at} /> },
            { label: "Event type", value: d.event_type ?? "—", mono: true },
            { label: "Event id", value: d.event_id ?? "—", mono: true },
            { label: "Detail", value: d.detail ?? "—" },
            { label: "Latency", value: ms(d.latency_us) },
            { label: "Client address", value: d.client_addr ?? "—", mono: true },
            { label: "Body size", value: `${d.body_size.toLocaleString()} bytes` },
            { label: "Replays", value: d.replays },
            { label: "Re-verifications", value: d.reverify_attempts },
          ]} />
          <section aria-label="Touched billing objects" className="space-y-1">
            <h3 className="text-sm font-semibold">Billing objects it touched</h3>
            {touched.length === 0 ? <p className="text-xs text-muted-foreground">None: the event changed nothing, or was not processed.</p> : (
              <ul aria-label="Touched objects" className="space-y-1 text-sm">
                {touched.map((t) => (
                  <li key={t.key} className="flex flex-wrap items-center gap-2">
                    <span className="text-muted-foreground">{t.label}</span>
                    {t.to ? <Link className="font-mono text-xs text-primary hover:underline" to={t.to} aria-label={`${t.label} ${t.id}`}>{t.id}</Link> : <span className="font-mono text-xs">{t.id}</span>}
                  </li>
                ))}
              </ul>
            )}
          </section>
          <section aria-label="Stored headers section" className="space-y-1">
            <h3 className="text-sm font-semibold">Headers</h3>
            <p className="text-xs text-muted-foreground">An allowlist: authorization, cookies and API keys are never stored.</p>
            <HeaderTable headers={d.headers} />
          </section>
          <section aria-label="Stored body" className="space-y-1">
            <h3 className="text-sm font-semibold">Body</h3>
            <Body d={d} />
          </section>
        </>
      )}
    </DetailSheet>
  );
}

/** Webhook deliveries (ADR-0032): every request a provider sent to ForgeBase, verified or not, to see why it was applied, ignored or refused. */
export default function Webhooks() {
  const [provider, setProvider] = useState(ANY);
  const [outcome, setOutcome] = useState(ANY);
  const [eventType, setEventType] = useState("");
  const [from, setFrom] = useState("");
  const [to, setTo] = useState("");
  const [page, setPage] = useState(1);
  const [open, setOpen] = useState<string>();
  const iso = (v: string) => (v && !Number.isNaN(new Date(v).getTime()) ? new Date(v).toISOString() : undefined);
  const filter = { provider: provider || undefined, outcome: outcome || undefined, event_type: eventType.trim() || undefined, from: iso(from), to: iso(to), limit: PER_PAGE, offset: (page - 1) * PER_PAGE };
  const list = useLoad((s) => listWebhookDeliveries(filter, s), `webhooks:${provider}:${outcome}:${filter.event_type ?? ""}:${filter.from ?? ""}:${filter.to ?? ""}:${page}`);
  const reset = <T,>(set: (v: T) => void) => (v: T) => { set(v); setPage(1); };
  const rows: WebhookDelivery[] = list.data?.items ?? [];
  const filtered = !!(provider || outcome || eventType.trim() || from || to);
  const toolbar = (
    <>
      <Filter label="Provider">{(id) => <Sel id={id} value={provider} onChange={reset(setProvider)} options={[{ value: ANY, label: "All providers" }, ...WEBHOOK_PROVIDERS.map((p) => ({ value: p, label: p.replace(/_/g, " ") }))]} className="w-44" />}</Filter>
      <Filter label="Outcome">{(id) => <Sel id={id} value={outcome} onChange={reset(setOutcome)} options={[{ value: ANY, label: "All outcomes" }, ...WEBHOOK_OUTCOMES.map((o) => ({ value: o, label: OUTCOME_WORDS[o] ?? o }))]} className="w-56" />}</Filter>
      <Filter label="Event type">{(id) => <Input id={id} size="sm" className="w-48 font-mono" value={eventType} onChange={(e) => reset(setEventType)(e.target.value)} placeholder="invoice.paid" />}</Filter>
      <Filter label="From">{(id) => <Input id={id} size="sm" type="datetime-local" className="w-52" value={from} onChange={(e) => reset(setFrom)(e.target.value)} />}</Filter>
      <Filter label="To">{(id) => <Input id={id} size="sm" type="datetime-local" className="w-52" value={to} onChange={(e) => reset(setTo)(e.target.value)} />}</Filter>
    </>
  );
  return (
    <div className="space-y-5">
      <BillingError error={list.error} />
      <TableCard
        title={<span className="inline-flex items-center gap-1">Webhook deliveries <HelpTip term="webhook_delivery" /></span>}
        count={list.data ? list.data.total : undefined}
        description="Every request a provider sent to ForgeBase, newest first, whether or not it verified. Kept for the retention you set in billing settings."
        actions={<Button onClick={list.reload}>Refresh</Button>}
        toolbar={toolbar}
        footer={list.data && list.data.total > PER_PAGE ? <Pager page={page} perPage={PER_PAGE} total={list.data.total} onPage={setPage} loading={list.loading} label="Webhook deliveries pages" /> : undefined}
      >
        {list.loading && !list.data && !list.error && <SkeletonTable rows={4} columns={7} label="Loading webhook deliveries" />}
        {list.data && rows.length === 0 && (
          filtered
            ? <EmptyState compact title="No deliveries match" why="Change or clear the filters to see more." />
            : <EmptyState title="No webhook deliveries yet" why="When a payment provider calls ForgeBase's webhook URL, the request is kept here with its headers, body and result. Set the URL in the provider's dashboard." />
        )}
        {rows.length > 0 && (
          <Table framed aria-label="Webhook deliveries">
            <Table.Header><Table.Row><Th>Received</Th><Th>Provider</Th><Th>Event type</Th><Th>Outcome</Th><Th>Detail</Th><Th align="end">Latency</Th><Th><span className="sr-only">Actions</span></Th></Table.Row></Table.Header>
            <Table.Body>
              {rows.map((d) => (
                <Table.Row key={d.id}>
                  <Table.Cell className="text-muted-foreground"><When iso={d.received_at} /></Table.Cell>
                  <Table.Cell className="capitalize">{d.provider.replace(/_/g, " ")}{d.account && <span className="block font-mono text-xs normal-case text-muted-foreground">{d.account}</span>}</Table.Cell>
                  <Table.Cell className="font-mono text-xs">{d.event_type ?? <None />}{d.event_id && <span className="block text-muted-foreground">{d.event_id}</span>}</Table.Cell>
                  <Table.Cell><Outcome outcome={d.outcome} /></Table.Cell>
                  <Table.Cell className="max-w-64 truncate text-xs text-muted-foreground" title={d.reason ?? d.detail ?? undefined}>{d.reason ?? d.detail ?? ""}</Table.Cell>
                  <Table.Cell align="end" className="tabular-nums">{ms(d.latency_us)}</Table.Cell>
                  <Table.Cell className="text-right"><Button aria-label={`Show delivery ${d.id}`} onClick={() => setOpen(d.id)}>Details</Button></Table.Cell>
                </Table.Row>
              ))}
            </Table.Body>
          </Table>
        )}
      </TableCard>
      <Drawer id={open} onClose={() => setOpen(undefined)} onChanged={list.reload} />
    </div>
  );
}
