import { useState } from "react";
import { deleteSlo, listSloStatus } from "../../api";
import type { BurnTier, Slo, SloStatus } from "../../api";
import { ConfirmDialog } from "../../components/ConfirmDialog";
import { EmptyState } from "../../components/EmptyState";
import { HelpTip } from "../../components/HelpTip";
import { SkeletonCards } from "../../components/Skeleton";
import { Badge } from "../../components/StatusBadge";
import { Button, ErrorView } from "../../components/ui";
import { useCreateParam, useLoad } from "../../hooks";
import { notify } from "../../toaster";
import { Section } from "../jobs/ops";
import SloSheet from "./SloSheet";
import { SeverityBadge } from "./shared";
import { fmtValue, pct } from "./util";

/** Windows in the order the server measures them; any other key follows. */
const WINDOWS = ["5m", "30m", "1h", "6h", "3d"];
const TIER_WORDS: Record<string, string> = { page_fast: "Fast page", page_slow: "Slow page", ticket: "Ticket" };

/** A burn rate of 1 spends the budget exactly over the window; the tone says how worrying it is. */
export function burnTone(rate: number | null): "ok" | "warn" | "bad" | "neutral" {
  if (rate === null) return "neutral";
  if (rate >= 6) return "bad";
  if (rate >= 1) return "warn";
  return "ok";
}

function BudgetBar({ ratio, name }: { ratio: number; name: string }) {
  const overspent = ratio < 0;
  const left = Math.max(0, Math.min(1, ratio));
  const tone = overspent || left < 0.1 ? "bg-destructive" : left < 0.4 ? "bg-warning" : "bg-success";
  return (
    <div>
      <div role="progressbar" aria-label={`Error budget remaining of ${name}`} aria-valuemin={0} aria-valuemax={100} aria-valuenow={Math.round(left * 100)} aria-valuetext={overspent ? `Overspent by ${pct(-ratio, 0)}` : `${pct(left, 0)} left`} className="h-2.5 w-full overflow-hidden rounded-full bg-muted">
        <div className={`h-full rounded-full ${tone}`} style={{ width: `${left * 100}%` }} />
      </div>
      <p className={`mt-1 text-sm font-medium ${overspent ? "text-destructive" : ""}`}>{overspent ? `Overspent by ${pct(-ratio, 0)}` : `${pct(left, 0)} of the error budget left`}</p>
    </div>
  );
}

function Tier({ t }: { t: BurnTier }) {
  return (
    <li className="flex flex-wrap items-center gap-2 text-sm">
      <span className="w-24 font-medium">{TIER_WORDS[t.tier] ?? t.tier}</span>
      <span className="text-muted-foreground">{t.long_window} and {t.short_window} above {t.burn_rate_threshold}×</span>
      <SeverityBadge severity={t.severity} />
      <Badge tone={t.firing ? "bad" : "ok"} dot pulse={t.firing}>{t.firing ? "firing" : "ok"}</Badge>
    </li>
  );
}

/** One SLO: SLI against the objective, the error budget, the burn rate of every window and its alert tiers. */
function SloCard({ status, onEdit, onDeleted }: { status: SloStatus; onEdit: (s: Slo) => void; onDeleted: () => void }) {
  const { slo, sli, total, good, bad, error_budget: eb, burn_rates: rates } = status;
  const meeting = sli !== null && sli >= slo.objective;
  const windows = [...WINDOWS.filter((w) => w in rates), ...Object.keys(rates).filter((w) => !WINDOWS.includes(w))];
  return (
    <article aria-label={`SLO ${slo.name}`} className="space-y-4 rounded-xl border border-border bg-card p-4">
      <header className="flex flex-wrap items-start justify-between gap-2">
        <div className="min-w-0">
          <h3 className="flex flex-wrap items-center gap-2 text-base font-semibold">{slo.name}<Badge>{slo.kind}</Badge>{!slo.alerting && <Badge tone="neutral">alerts off</Badge>}</h3>
          <p className="text-sm text-muted-foreground">
            {pct(slo.objective, 3)} over {slo.window}
            {slo.kind === "latency" && slo.latency_threshold_ms ? `, answered within ${slo.latency_threshold_ms} ms` : ""}
            {slo.route_prefix ? ` · routes starting ${slo.route_prefix}` : slo.route_regex ? ` · routes matching ${slo.route_regex}` : " · every route"}
            {slo.methods.length ? ` · ${slo.methods.join(", ")}` : ""}
          </p>
          {slo.description && <p className="text-sm">{slo.description}</p>}
        </div>
        <div className="flex gap-2">
          <Button aria-label={`Edit ${slo.name}`} onClick={() => onEdit(slo)}>Edit</Button>
          <ConfirmDialog
            trigger={<Button tone="danger" aria-label={`Delete ${slo.name}`}>Delete</Button>}
            title={`Delete SLO ${slo.name}?`}
            consequence="Its counts and its burn-rate rules are deleted. Past alerts stay in the history."
            confirmLabel="Delete SLO"
            onConfirm={async () => { await deleteSlo(slo.id); notify.deleted(`SLO ${slo.name}`); onDeleted(); }}
          />
        </div>
      </header>

      {total === 0 ? (
        <p className="rounded-lg border border-border bg-muted/40 p-3 text-sm text-muted-foreground">No matching requests in the window yet, so there is no SLI. The budget is whole until traffic arrives.</p>
      ) : (
        <div className="grid gap-4 md:grid-cols-2">
          <div>
            <p className="text-xs font-medium uppercase tracking-wide text-muted-foreground">SLI <span className="normal-case">(good ÷ all requests)</span></p>
            <p className="flex flex-wrap items-baseline gap-2">
              <span aria-label={`SLI of ${slo.name}`} className="text-2xl font-semibold tabular-nums">{pct(sli, 3)}</span>
              <span className="text-sm text-muted-foreground">objective {pct(slo.objective, 3)}</span>
              <Badge tone={meeting ? "ok" : "bad"} dot>{meeting ? "meeting objective" : "below objective"}</Badge>
            </p>
            <p className="mt-1 text-xs text-muted-foreground">{total.toLocaleString()} requests: {good.toLocaleString()} good, {bad.toLocaleString()} bad.</p>
          </div>
          <div>
            <p className="mb-1 text-xs font-medium uppercase tracking-wide text-muted-foreground">Error budget <HelpTip term="error_budget" /></p>
            <BudgetBar ratio={eb.remaining_ratio} name={slo.name} />
            <p className="mt-0.5 text-xs text-muted-foreground">{eb.consumed.toLocaleString()} of {fmtValue(Math.round(eb.allowed * 10) / 10)} allowed bad requests used.</p>
          </div>
        </div>
      )}

      <div className="grid gap-4 md:grid-cols-2">
        <section aria-label={`Burn rates of ${slo.name}`}>
          <h4 className="mb-1 text-sm font-medium">Burn rate <HelpTip term="burn_rate" /></h4>
          <table className="w-full text-sm">
            <tbody>
              {windows.map((w) => {
                const r = rates[w] ?? null;
                return (
                  <tr key={w} className="border-t border-border first:border-t-0">
                    <th scope="row" className="py-1 text-left font-normal text-muted-foreground">{w}</th>
                    <td className="py-1 text-right tabular-nums"><Badge tone={burnTone(r)}>{r === null ? "no data" : `${fmtValue(Math.round(r * 100) / 100)}×`}</Badge></td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </section>
        <section aria-label={`Alert tiers of ${slo.name}`}>
          <h4 className="mb-1 text-sm font-medium">Burn-rate alerts</h4>
          {status.alerts.length === 0 ? <p className="text-sm text-muted-foreground">{slo.alerting ? "No tier fits the window." : "Alerting is off for this SLO."}</p> : <ul className="space-y-1">{status.alerts.map((t) => <Tier key={t.rule_id} t={t} />)}</ul>}
        </section>
      </div>
    </article>
  );
}

/** SLOs with a status card each, and create, change and delete. */
export default function Slos() {
  const [creating, setCreating] = useCreateParam();
  const list = useLoad(listSloStatus, "slo-status");
  const [editing, setEditing] = useState<Slo>();
  const items = list.data ?? [];
  return (
    <>
      <Section title="Service level objectives" count={list.data?.length} description="Availability and latency promises over a rolling window, with the error budget they leave." actions={<Button onClick={list.reload}>Refresh</Button>}>
        <ErrorView error={list.error} onRetry={list.reload} />
        {list.loading && !list.data && <SkeletonCards count={2} label="Loading SLOs" />}
        {list.data && items.length === 0 && <EmptyState compact title="No SLOs yet" why="An SLO turns “the API should be reliable” into a number, an error budget and alerts that fire before the budget is gone." action={<Button tone="primary" onClick={() => setCreating(true)}>New SLO</Button>} />}
        <div className="space-y-4">
          {items.map((s) => <SloCard key={s.slo.id} status={s} onEdit={setEditing} onDeleted={list.reload} />)}
        </div>
      </Section>
      {creating && <SloSheet open onOpenChange={setCreating} onSaved={list.reload} />}
      {editing && <SloSheet key={editing.id} open slo={editing} onOpenChange={(o) => !o && setEditing(undefined)} onSaved={list.reload} />}
    </>
  );
}
