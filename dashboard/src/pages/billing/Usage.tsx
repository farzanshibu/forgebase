import { useState } from "react";
import { listUsage, listUsageAlerts } from "../../api";
import type { UsageSummary } from "../../api";
import { Button, Loading, Panel, inputCls } from "../../components/ui";
import { useLoad } from "../../hooks";
import { formatCount, meterState, percent } from "./money";
import type { MeterState } from "./money";
import { BillingError, Empty, ROW, TABLE, THEAD, Th, when_ } from "./shared";

const BAR: Record<MeterState, string> = { ok: "bg-emerald-500", warn70: "bg-yellow-400", warn90: "bg-orange-500", full: "bg-red-500" };
const LABEL: Record<MeterState, string> = { ok: "within quota", warn70: "70% used", warn90: "90% used", full: "quota reached" };

export function Meter({ u }: { u: UsageSummary }) {
  const state = meterState(u.used, u.limit);
  const pct = percent(u.used, u.limit);
  return (
    <li className="rounded-lg border border-zinc-800 bg-zinc-900/60 p-3" data-state={state}>
      <div className="flex items-baseline justify-between gap-3">
        <span className="font-medium">{u.feature}</span>
        <span aria-label={`${u.feature} usage`} className="tabular-nums text-sm">{formatCount(u.used)} / {u.limit === null ? "unlimited" : formatCount(u.limit)}</span>
      </div>
      {u.limit !== null && (
        <div role="progressbar" aria-label={`${u.feature} quota`} aria-valuenow={pct} aria-valuemin={0} aria-valuemax={100} className="mt-2 h-2 overflow-hidden rounded bg-zinc-800">
          <div className={`h-full ${BAR[state]}`} style={{ width: `${pct}%` }} />
        </div>
      )}
      <p className="mt-1 text-xs text-zinc-400">
        {LABEL[state]} · {u.enforcement} limit · {u.period}{u.pack_remaining > 0 ? ` · ${formatCount(u.pack_remaining)} from usage packs` : ""}{u.reserved > 0 ? ` · ${formatCount(u.reserved)} reserved` : ""}
      </p>
    </li>
  );
}

export default function Usage() {
  const [input, setInput] = useState("");
  const [subject, setSubject] = useState("");
  const usage = useLoad((s) => (subject ? listUsage(subject, s) : Promise.resolve(undefined)), `usage:${subject}`);
  const alerts = useLoad((s) => (subject ? listUsageAlerts(subject, s) : Promise.resolve(undefined)), `alerts:${subject}`);
  return (
    <div className="space-y-4">
      <form className="flex items-end gap-2 text-sm" onSubmit={(e) => { e.preventDefault(); setSubject(input.trim()); }}>
        <label>Usage subject<input className={`${inputCls} ml-2 w-56`} value={input} onChange={(e) => setInput(e.target.value)} placeholder="org:acme or a user key" /></label>
        <Button type="submit">Show usage</Button>
      </form>
      {!subject && <Empty>Enter a subject to see its usage meters against plan quotas.</Empty>}
      <BillingError error={usage.error} />
      {subject && usage.loading && !usage.error && <Loading what="Loading usage" />}
      {usage.data && usage.data.length === 0 && <Empty>No metered features for this subject.</Empty>}
      {usage.data && usage.data.length > 0 && <ul aria-label="Usage meters" className="grid gap-3 md:grid-cols-2">{usage.data.map((u) => <Meter key={`${u.feature}:${u.scope_key}`} u={u} />)}</ul>}
      {alerts.data && alerts.data.length > 0 && (
        <Panel title="Quota alerts">
          <table aria-label="Quota alerts" className={TABLE}>
            <thead className={THEAD}><tr><Th>When</Th><Th>Feature</Th><Th>Threshold</Th><Th>Used</Th></tr></thead>
            <tbody>{alerts.data.map((a) => <tr key={a.id} className={ROW}><td className="py-1.5 pr-3 text-zinc-400">{when_(a.at)}</td><td>{a.feature}</td><td>{a.threshold}%</td><td className="tabular-nums">{formatCount(a.used)} / {formatCount(a.limit)}</td></tr>)}</tbody>
          </table>
        </Panel>
      )}
    </div>
  );
}
