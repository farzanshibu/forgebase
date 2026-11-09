import { useState } from "react";
import { SUBSCRIPTION_STATUSES, cancelSubscription, changePlan, listPrices, previewPlanChange, listSubscriptions, pauseSubscription, resumeSubscription, subscribe } from "../../api";
import type { Price, Proration, Subscription, SubscriptionStatus } from "../../api";
import { Button, ConfirmButton, ErrorView, Loading, Panel, inputCls } from "../../components/ui";
import { useLoad } from "../../hooks";
import { formatMoney } from "./money";
import { Badge, BillingError, Empty, Field, Th, SubjectLink } from "./shared";
import { FORM_GRID, ROW, TABLE, THEAD, idempotencyKey, when_ } from "./util";
import { useAct } from "./useAct";

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
        <div className="space-y-2 rounded border border-zinc-800 p-3" role="group" aria-label={`Change plan of ${sub.id}`}>
          <div className={FORM_GRID}>
            <Field label="New price">
              <select className={inputCls} value={target} onChange={(e) => setTarget(e.target.value)}>
                <option value="">choose…</option>{candidates.map((p) => <option key={p.key} value={p.key}>{p.key}</option>)}
              </select>
            </Field>
            <Field label="Proration">
              <select className={inputCls} value={proration} onChange={(e) => setProration(e.target.value as Proration)}>
                <option value="create_prorations">create prorations</option><option value="always_invoice">invoice now</option><option value="none">none</option>
              </select>
            </Field>
            <label className="flex items-center gap-2 pt-6 text-sm text-zinc-300"><input type="checkbox" checked={atEnd} onChange={(e) => setAtEnd(e.target.checked)} />At period end</label>
          </div>
          {preview.error != null && <ErrorView error={preview.error} />}
          {preview.data && (
            <p aria-label="Proration preview" className="text-sm text-zinc-300">
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
      {act.note && <p role="status" className="text-sm text-emerald-300">{act.note}</p>}
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
        <Field label="Subscriber (org:<id> or user key)"><input className={inputCls} value={subject} onChange={(e) => setSubject(e.target.value)} required /></Field>
        <Field label="Recurring price"><select className={inputCls} value={price} onChange={(e) => setPrice(e.target.value)} required><option value="">choose…</option>{prices.filter((p) => p.type !== "one_time").map((p) => <option key={p.key} value={p.key}>{p.key}</option>)}</select></Field>
        <Field label="Trial days (blank: price default)"><input className={inputCls} inputMode="numeric" value={trial} onChange={(e) => setTrial(e.target.value)} /></Field>
        <Field label="Coupon"><input className={inputCls} value={coupon} onChange={(e) => setCoupon(e.target.value)} /></Field>
      </div>
      <ErrorView error={act.error} />
      {act.note && <p role="status" className="text-sm text-emerald-300">{act.note}</p>}
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
          <select className={`${inputCls} ml-2`} value={status} onChange={(e) => setStatus(e.target.value as SubscriptionStatus | "")}>
            <option value="">all</option>{SUBSCRIPTION_STATUSES.map((s) => <option key={s}>{s}</option>)}
          </select>
        </label>
        <label>Subject<input className={`${inputCls} ml-2 w-56`} value={subject} onChange={(e) => setSubject(e.target.value)} placeholder="all subjects" /></label>
        <Button onClick={subs.reload}>Refresh</Button>
      </div>
      <BillingError error={subs.error} />
      {subs.loading && !subs.error && <Loading what="Loading subscriptions" />}
      {subs.data && rows.length === 0 && <Empty>No subscriptions match.</Empty>}
      {rows.length > 0 && (
        <table aria-label="Subscriptions" className={TABLE}>
          <thead className={THEAD}><tr><Th>Subscriber</Th><Th>Plan</Th><Th>Status</Th><Th>Period</Th><Th>Trial ends</Th><Th /></tr></thead>
          <tbody>
            {rows.map((s) => (
              <tr key={s.id} className={`${ROW} align-top`}>
                <td className="py-2 pr-3"><SubjectLink subject={s.subject} /></td>
                <td className="pr-3 font-mono text-xs">{s.items.map((i) => `${i.price}×${i.quantity}`).join(", ")}{s.pending_price && <span className="ml-1 text-sky-300">→ {s.pending_price}</span>}</td>
                <td className="pr-3"><Badge status={s.status} />{s.cancel_at_period_end && <span className="ml-1 text-xs text-amber-300">ends at period end</span>}</td>
                <td className="pr-3 text-zinc-400">{when_(s.current_period_start)} to {when_(s.current_period_end)}</td>
                <td className="pr-3 text-zinc-400">{s.trial_ends_at ? when_(s.trial_ends_at) : "-"}</td>
                <td><Button aria-label={`Manage ${s.id}`} onClick={() => setOpen(open === s.id ? undefined : s.id)}>Manage</Button></td>
              </tr>
            ))}
          </tbody>
        </table>
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
