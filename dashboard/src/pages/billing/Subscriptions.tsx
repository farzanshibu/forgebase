import { useState } from "react";
import { EmptyState } from "../../components/EmptyState";
import { SUBSCRIPTION_STATUSES, cancelSubscription, changePlan, listPrices, previewPlanChange, listSubscriptions, pauseSubscription, resumeSubscription, subscribe } from "../../api";
import type { Price, Proration, Subscription, SubscriptionStatus } from "../../api";
import { Button, ConfirmButton, ErrorView } from "../../components/ui";
import { Check, Sel } from "../../components/controls";
import { CreateSheet, DetailSheet } from "../../components/CreateSheet";
import { FormField } from "../../components/FormField";
import { SkeletonTable } from "../../components/Skeleton";
import { useLoad } from "../../hooks";
import { formatMoney } from "./money";
import { BillingError, Field, Filter, None, Note, SubjectLink, TableCard } from "./shared";
import { FORM_GRID, idempotencyKey, when_, words } from "./util";
import { useAct } from "./useAct";
import { Table } from "panelui-web/table";
import { Input } from "panelui-web/input";
import { Badge } from "../../components/StatusBadge";
import { Th } from "../../components/TableCard";
import { When } from "../../components/Time";

const plan = (s: Subscription) => s.items.map((i) => `${i.price}×${i.quantity}`).join(", ");

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
    <div className="space-y-5">
      <dl aria-label="Subscription summary" className="grid grid-cols-2 gap-x-4 gap-y-3 rounded-lg border border-border p-3 text-sm">
        <div><dt className="text-xs text-muted-foreground">Status</dt><dd className="mt-0.5 flex flex-wrap items-center gap-1.5"><Badge status={sub.status} />{sub.cancel_at_period_end && <span className="text-xs text-warning">cancels when this period ends</span>}</dd></div>
        <div><dt className="text-xs text-muted-foreground">Subscriber</dt><dd className="mt-0.5"><SubjectLink subject={sub.subject} /></dd></div>
        <div><dt className="text-xs text-muted-foreground">Plan</dt><dd className="mt-0.5 font-mono text-xs">{plan(sub)}{sub.pending_price && <span className="ml-1 text-info">→ {sub.pending_price}</span>}</dd></div>
        <div><dt className="text-xs text-muted-foreground">Current period</dt><dd className="mt-0.5"><When iso={sub.current_period_start} day /> to <When iso={sub.current_period_end} day /></dd></div>
        {sub.trial_ends_at && <div><dt className="text-xs text-muted-foreground">Trial ends</dt><dd className="mt-0.5"><When iso={sub.trial_ends_at} day /></dd></div>}
      </dl>
      {live && (
        <section aria-label="Actions" className="space-y-2">
          <h3 className="text-sm font-semibold">Actions</h3>
          <div className="flex flex-wrap gap-2">
            {sub.status === "active" && <Button disabled={act.busy} onClick={() => act.run(() => pauseSubscription(sub.id), "Paused")}>Pause</Button>}
            {(sub.status === "paused" || sub.cancel_at_period_end) && <Button disabled={act.busy} onClick={() => act.run(() => resumeSubscription(sub.id), "Resumed")}>{sub.cancel_at_period_end ? "Keep subscription" : "Resume"}</Button>}
            <Button aria-expanded={changing} onClick={() => setChanging((v) => !v)}>Change plan</Button>
            {!sub.cancel_at_period_end && <Button disabled={act.busy} onClick={() => act.run(() => cancelSubscription(sub.id, { at_period_end: true }), "Cancels at period end")}>Cancel at period end</Button>}
            <ConfirmButton label="Cancel now" short="Cancel now" consequence="The subscription ends at once and the customer loses access. This cannot be undone." disabled={act.busy} onConfirm={() => void act.run(() => cancelSubscription(sub.id, { at_period_end: false, reason: "cancelled by operator" }), "Cancelled")} />
          </div>
        </section>
      )}
      {!live && <p className="text-sm text-muted-foreground">This subscription is canceled. Start a new one to bill this customer again.</p>}
      {changing && (
        <div className="space-y-3 rounded-lg border border-border bg-muted/30 p-3" role="group" aria-label={`Change plan of ${sub.id}`}>
          <div className={FORM_GRID}>
            <Field label="New price">
              <Sel value={target} onChange={setTarget} placeholder="Choose a price" options={candidates.map((p) => ({ value: p.key, label: p.key }))} />
            </Field>
            <Field label="Proration">
              <Sel value={proration} onChange={(v) => setProration(v as Proration)} options={[{ value: "create_prorations", label: "Prorate on next invoice" }, { value: "always_invoice", label: "Prorate and invoice now" }, { value: "none", label: "No proration" }]} />
            </Field>
          </div>
          <Check checked={atEnd} onChange={setAtEnd} label="At period end" description="Switch when the current period ends instead of now." />
          {preview.error != null && <ErrorView error={preview.error} />}
          {preview.data && (
            <p aria-label="Proration preview" className="rounded-md border border-border bg-card p-2 text-sm text-foreground/80">
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
      <Note>{act.note}</Note>
    </div>
  );
}

function NewSubscription({ open, onOpenChange, prices, onDone }: { open: boolean; onOpenChange: (v: boolean) => void; prices: Price[]; onDone: () => void }) {
  const [subject, setSubject] = useState("");
  const [price, setPrice] = useState("");
  const [trial, setTrial] = useState("");
  const [coupon, setCoupon] = useState("");
  const trialBad = trial !== "" && !/^\d+$/.test(trial.trim());
  return (
    <CreateSheet
      open={open}
      onOpenChange={onOpenChange}
      title="New subscription"
      description="Start a subscription or a trial for a user or an organization."
      submitLabel="Start subscription"
      canSubmit={!!subject.trim() && !!price && !trialBad}
      onSubmit={async () => {
        await subscribe({ subject: subject.trim(), items: [{ price, quantity: 1 }], trial_days: trial ? Number(trial) : null, coupon: coupon || null, idempotency_key: idempotencyKey("sub") });
        onDone();
        const what = `Subscription for ${subject.trim()}`;
        setSubject(""); setPrice(""); setTrial(""); setCoupon("");
        return { what };
      }}
    >
      <FormField label="Subscriber" help="org:<id> for an organization, or a user key.">
        {(p) => <Input {...p} size="sm" className="font-mono" value={subject} onChange={(e) => setSubject(e.target.value)} placeholder="org:acme" />}
      </FormField>
      <FormField label="Recurring price">
        {(p) => <Sel {...p} value={price} onChange={setPrice} placeholder="Choose a price" options={prices.filter((x) => x.type !== "one_time" && x.active).map((x) => ({ value: x.key, label: `${x.key} (${words(x.type)})` }))} />}
      </FormField>
      <div className={FORM_GRID}>
        <FormField label="Trial days" help="Blank uses the price's default." error={trialBad ? "A whole number of days" : undefined}>
          {(p) => <Input {...p} size="sm" inputMode="numeric" value={trial} onChange={(e) => setTrial(e.target.value)} />}
        </FormField>
        <FormField label="Coupon" help="Optional coupon code.">
          {(p) => <Input {...p} size="sm" className="font-mono uppercase" value={coupon} onChange={(e) => setCoupon(e.target.value)} />}
        </FormField>
      </div>
    </CreateSheet>
  );
}

export default function Subscriptions() {
  const [status, setStatus] = useState<SubscriptionStatus | "">("");
  const [subject, setSubject] = useState("");
  const subs = useLoad((s) => listSubscriptions(subject || undefined, s), `subs:${subject}`);
  const prices = useLoad((s) => listPrices(undefined, s), "sub-prices");
  const rows = (subs.data ?? []).filter((s) => !status || s.status === status);
  const [open, setOpen] = useState<string>();
  const [creating, setCreating] = useState(false);
  const managed = (subs.data ?? []).find((s) => s.id === open);
  const add = <Button tone="primary" onClick={() => setCreating(true)}>New subscription</Button>;
  const toolbar = (
    <>
      <Filter label="Status">
        {(id) => <Sel placeholder="All statuses" id={id} value={status} onChange={(v) => setStatus(v as SubscriptionStatus | "")} options={[{ value: "", label: "All statuses" }, ...SUBSCRIPTION_STATUSES.map((s) => ({ value: s, label: words(s) }))]} className="w-40" />}
      </Filter>
      <Filter label="Subject">
        {(id) => <Input id={id} size="sm" className="w-56 font-mono" value={subject} onChange={(e) => setSubject(e.target.value)} placeholder="All subjects" />}
      </Filter>
      <Button className="ml-auto" onClick={subs.reload} disabled={subs.loading}>Refresh</Button>
    </>
  );
  return (
    <div className="space-y-4">
      <BillingError error={subs.error} />
      <TableCard title="Subscriptions" count={subs.data ? rows.length : undefined} description="Recurring plans, their status and the current billing period." actions={add} toolbar={toolbar}>
        {subs.loading && !subs.error && <SkeletonTable rows={4} columns={5} label="Loading subscriptions" />}
        {subs.data && rows.length === 0 && (
          subs.data.length === 0 && !subject
            ? <EmptyState title="No subscriptions yet" why="A subscription bills a customer for a recurring price every month or year, with an optional trial." action={add} />
            : <EmptyState compact title="No subscriptions match" why="Change the status or subject filter to see more." />
        )}
        {rows.length > 0 && (
          <Table framed aria-label="Subscriptions">
            <Table.Header><Table.Row><Th>Subscriber</Th><Th>Plan</Th><Th>Status</Th><Th>Current period</Th><Th>Trial ends</Th><Th><span className="sr-only">Actions</span></Th></Table.Row></Table.Header>
            <Table.Body>
              {rows.map((s) => (
                <Table.Row key={s.id}>
                  <Table.Cell><SubjectLink subject={s.subject} /></Table.Cell>
                  <Table.Cell className="font-mono text-xs">{plan(s)}{s.pending_price && <span className="ml-1 text-info">→ {s.pending_price}</span>}</Table.Cell>
                  <Table.Cell>
                    <span className="flex flex-wrap items-center gap-1.5"><Badge status={s.status} />{s.cancel_at_period_end && <span className="text-xs text-warning">ends at period end</span>}</span>
                  </Table.Cell>
                  <Table.Cell className="whitespace-nowrap text-muted-foreground"><When iso={s.current_period_start} day /> – <When iso={s.current_period_end} day /></Table.Cell>
                  <Table.Cell className="text-muted-foreground">{s.trial_ends_at ? <When iso={s.trial_ends_at} day /> : <None />}</Table.Cell>
                  <Table.Cell className="text-right"><Button aria-label={`Manage ${s.id}`} onClick={() => setOpen(s.id)}>Manage</Button></Table.Cell>
                </Table.Row>
              ))}
            </Table.Body>
          </Table>
        )}
      </TableCard>
      <DetailSheet open={!!managed} onOpenChange={(v) => { if (!v) setOpen(undefined); }} title={<span className="font-mono">{managed?.id}</span>} description="Subscription">
        {managed && <Lifecycle key={managed.id} sub={managed} prices={prices.data ?? []} onDone={subs.reload} />}
      </DetailSheet>
      <NewSubscription open={creating} onOpenChange={setCreating} prices={prices.data ?? []} onDone={subs.reload} />
    </div>
  );
}
