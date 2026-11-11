import { useState } from "react";
import { EmptyState } from "../../components/EmptyState";
import { SUBSCRIPTION_STATUSES, cancelSubscription, changePlan, listPrices, previewPlanChange, listSubscriptions, pauseSubscription, resumeSubscription, subscribe } from "../../api";
import type { Price, Proration, Subscription, SubscriptionStatus } from "../../api";
import { Button, ConfirmButton, ErrorView, Loading, Panel } from "../../components/ui";
import { Sel } from "../../components/controls";
import { useLoad } from "../../hooks";
import { formatMoney } from "./money";
import { Badge, BillingError, Field, Th, SubjectLink } from "./shared";
import { FORM_GRID, ROW, TABLE, THEAD, idempotencyKey, when_ } from "./util";
import { useAct } from "./useAct";
import { Table } from "panelui-web/table";
import { Input } from "panelui-web/input";

function Lifecycle({ sub, prices, onDone }: { sub: Subscription; prices: Price[]; onDone: () => void }) {
  const act = useAct(onDone);
  const [changing, setChanging] = useState(false);
  const [target, setTarget] = useState("");
  const [proration, setProration] = useState<Proration>("create_prorations");
  const [atEnd, setAtEnd] = useState(false);
  const live = sub.status !== "canceled";
  const current = prices.find((p) => p.key === sub.items[0]?.price);
  const preview = useLoad(
    (s) => (changing && target ? previewPlanChange(sub.id, { price: target, proration, at_period_end: atEnd }, s) : Promise.resolve(undefined)),
    `preview:${sub.id}:${changing}:${target}:${proration}:${atEnd}`,
  );
  const candidates = prices.filter((p) => p.type === "recurring" && p.active && p.key !== current?.key);
  return (
    <div className="space-y-2">
      <div className="flex flex-wrap gap-2">
        {sub.status === "active" && <Button disabled={act.busy} onClick={() => act.run(() => pauseSubscription(sub.id), "Paused")}>Pause</Button>}
        {(sub.status === "paused" || sub.cancel_at_period_end) && <Button disabled={act.busy} onClick={() => act.run(() => resumeSubscription(sub.id), "Resumed")}>{sub.cancel_at_period_end ? "Keep subscription" : "Resume"}</Button>}
        {live && !sub.cancel_at_period_end && <Button disabled={act.busy} onClick={() => act.run(() => cancelSubscription(sub.id, { at_period_end: true }), "Cancels at period end")}>Cancel at period end</Button>}
        {live && <ConfirmButton label="Cancel now" disabled={act.busy} onConfirm={() => void act.run(() => cancelSubscription(sub.id, { at_period_end: false, reason: "cancelled by operator" }), "Cancelled")} />}
        {live && <Button onClick={() => setChanging((v) => !v)}>Change plan</Button>}
      </div>
      {changing && (
        <div className="space-y-2 rounded border border-border p-3" role="group" aria-label={`Change plan of ${sub.id}`}>
          <div className={FORM_GRID}>
            <Field label="New price">
              <Sel value={target} onChange={setTarget} options={[{ value: "", label: "choose…" }, ...candidates.map((p) => ({ value: p.key, label: p.key }))]} />
            </Field>
            <Field label="Proration">
              <Sel value={proration} onChange={(v) => setProration(v as Proration)} options={[{ value: "create_prorations", label: "create prorations" }, { value: "always_invoice", label: "invoice now" }, "none"]} />
            </Field>
            <label className="flex items-center gap-2 pt-6 text-sm text-foreground/80"><input type="checkbox" checked={atEnd} onChange={(e) => setAtEnd(e.target.checked)} />At period end</label>
          </div>
          {preview.error != null && <ErrorView error={preview.error} />}
          {preview.data && (
            <p aria-label="Proration preview" className="text-sm text-foreground/80">
              {preview.data.at_period_end
                ? `Takes effect ${when_(preview.data.effective_at)}; nothing is charged now. The next period costs ${formatMoney(preview.data.new_period_amount, preview.data.currency)}.`
                : preview.data.proration === "none"
                  ? `No proration. The next period costs ${formatMoney(preview.data.new_period_amount, preview.data.currency)}.`
                  : `Unused credit ${formatMoney(preview.data.credit, preview.data.currency)}, new charge ${formatMoney(preview.data.charge, preview.data.currency)}, net ${formatMoney(preview.data.net, preview.data.currency)}${preview.data.invoiced_now ? " (invoiced now)" : " (added to the next invoice)"}.`}
            </p>
          )}
          <Button tone="primary" disabled={!target || act.busy} onClick={() => act.run(() => changePlan(sub.id, { price: target, proration, at_period_end: atEnd }), "Plan changed")}>Apply plan change</Button>
        </div>
      )}
      <ErrorView error={act.error} />
      {act.note && <p role="status" className="text-sm text-success">{act.note}</p>}
    </div>
  );
}

function NewSubscription({ prices, onDone }: { prices: Price[]; onDone: () => void }) {
  const act = useAct(onDone);
  const [subject, setSubject] = useState("");
  const [price, setPrice] = useState("");
  const [trial, setTrial] = useState("");
  const [coupon, setCoupon] = useState("");
  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    await act.run(() => subscribe({ subject, items: [{ price, quantity: 1 }], trial_days: trial ? Number(trial) : null, coupon: coupon || null, idempotency_key: idempotencyKey("sub") }), "Subscribed");
  };
  return (
    <form onSubmit={submit} aria-label="New subscription" className="space-y-3">
      <div className={FORM_GRID}>
        <Field label="Subscriber (org:<id> or user key)"><Input size="sm" value={subject} onChange={(e) => setSubject(e.target.value)} required /></Field>
        <Field label="Recurring price"><Sel value={price} onChange={setPrice} options={[{ value: "", label: "choose…" }, ...prices.filter((p) => p.type !== "one_time").map((p) => ({ value: p.key, label: p.key }))]} required /></Field>
        <Field label="Trial days (blank: price default)"><Input size="sm" inputMode="numeric" value={trial} onChange={(e) => setTrial(e.target.value)} /></Field>
        <Field label="Coupon"><Input size="sm" value={coupon} onChange={(e) => setCoupon(e.target.value)} /></Field>
      </div>
      <ErrorView error={act.error} />
      {act.note && <p role="status" className="text-sm text-success">{act.note}</p>}
      <Button type="submit" tone="primary" disabled={act.busy}>Start subscription</Button>
    </form>
  );
}

export default function Subscriptions() {
  const [status, setStatus] = useState<SubscriptionStatus | "">("");
  const [subject, setSubject] = useState("");
  const subs = useLoad((s) => listSubscriptions(subject || undefined, s), `subs:${subject}`);
  const prices = useLoad((s) => listPrices(undefined, s), "sub-prices");
  const rows = (subs.data ?? []).filter((s) => !status || s.status === status);
  const [open, setOpen] = useState<string>();
  return (
    <div className="space-y-4">
      <div className="flex flex-wrap items-end gap-3 text-sm">
        <label>Status
          <Sel value={status} onChange={(v) => setStatus(v as SubscriptionStatus | "")} options={[{ value: "", label: "all" }, ...SUBSCRIPTION_STATUSES]} className="ml-2" />
        </label>
        <label>Subject<Input size="sm" className="ml-2 w-56" value={subject} onChange={(e) => setSubject(e.target.value)} placeholder="all subjects" /></label>
        <Button onClick={subs.reload}>Refresh</Button>
      </div>
      <BillingError error={subs.error} />
      {subs.loading && !subs.error && <Loading what="Loading subscriptions" />}
      {subs.data && rows.length === 0 && <EmptyState compact title="No subscriptions match." why="Change the filters to see more." />}
      {rows.length > 0 && (
        <Table aria-label="Subscriptions" className={TABLE}>
          <Table.Header className={THEAD}><Table.Row><Th>Subscriber</Th><Th>Plan</Th><Th>Status</Th><Th>Period</Th><Th>Trial ends</Th><Th /></Table.Row></Table.Header>
          <Table.Body>
            {rows.map((s) => (
              <Table.Row key={s.id} className={`${ROW} align-top`}>
                <Table.Cell className="py-2 pr-3"><SubjectLink subject={s.subject} /></Table.Cell>
                <Table.Cell className="pr-3 font-mono text-xs">{s.items.map((i) => `${i.price}×${i.quantity}`).join(", ")}{s.pending_price && <span className="ml-1 text-info">→ {s.pending_price}</span>}</Table.Cell>
                <Table.Cell className="pr-3"><Badge status={s.status} />{s.cancel_at_period_end && <span className="ml-1 text-xs text-warning">ends at period end</span>}</Table.Cell>
                <Table.Cell className="pr-3 text-muted-foreground">{when_(s.current_period_start)} to {when_(s.current_period_end)}</Table.Cell>
                <Table.Cell className="pr-3 text-muted-foreground">{s.trial_ends_at ? when_(s.trial_ends_at) : "-"}</Table.Cell>
                <Table.Cell><Button aria-label={`Manage ${s.id}`} onClick={() => setOpen(open === s.id ? undefined : s.id)}>Manage</Button></Table.Cell>
              </Table.Row>
            ))}
          </Table.Body>
        </Table>
      )}
      {rows.filter((s) => s.id === open).map((s) => (
        <Panel key={s.id} title={`Subscription ${s.id}`}>
          <Lifecycle sub={s} prices={prices.data ?? []} onDone={subs.reload} />
        </Panel>
      ))}
      <Panel title="Start a subscription or trial"><NewSubscription prices={prices.data ?? []} onDone={subs.reload} /></Panel>
    </div>
  );
}
