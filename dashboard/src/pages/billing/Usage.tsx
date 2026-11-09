import { useState } from "react";
import { listUsageAlerts, listUsageMeters } from "../../api";
import type { ServerMeterState, UsageMeter } from "../../api";
import { Button, Loading, Panel, inputCls } from "../../components/ui";
import { useLoad } from "../../hooks";
import { formatCount, percent } from "./money";
import { BillingError, Empty, SubjectLink, Th } from "./shared";
import { ROW, TABLE, THEAD, when_ } from "./util";

const PAGE = 50;
const BAR: Record<ServerMeterState, string> = { ok: "bg-emerald-500", warn: "bg-yellow-400", critical: "bg-orange-500", over: "bg-red-500" };
const LABEL: Record<ServerMeterState, string> = { ok: "within quota", warn: "70% used", critical: "90% used", over: "quota reached" };
/** `data-state` values kept from the first version of this page. */
const DATA_STATE: Record<ServerMeterState, string> = { ok: "ok", warn: "warn70", critical: "warn90", over: "full" };

export function Meter({ u }: { u: UsageMeter }) {
  const pct = u.limit === null ? 0 : percent(u.used, u.limit);
  return (
    <li className="rounded-lg border border-zinc-800 bg-zinc-900/60 p-3" data-state={DATA_STATE[u.state]}>
      <div className="flex items-baseline justify-between gap-3">
        <span className="font-medium">{u.feature}</span>
        <span aria-label={`${u.feature} usage`} className="tabular-nums text-sm">{formatCount(u.used)} / {u.limit === null ? "unlimited" : formatCount(u.limit)}</span>
      </div>
      {u.limit !== null && (
        <div role="progressbar" aria-label={`${u.feature} quota`} aria-valuenow={pct} aria-valuemin={0} aria-valuemax={100} className="mt-2 h-2 overflow-hidden rounded bg-zinc-800">
          <div className={`h-full ${BAR[u.state]}`} style={{ width: `${pct}%` }} />
        </div>
      )}
      <p className="mt-1 text-xs text-zinc-400">
        <SubjectLink subject={u.subject} /> · {LABEL[u.state]} · window {when_(u.window_start)} to {when_(u.window_end)}{u.reserved > 0 ? ` · ${formatCount(u.reserved)} reserved` : ""}{u.carried_in > 0 ? ` · ${formatCount(u.carried_in)} carried in` : ""}
      </p>
    </li>
  );
}

export default function Usage() {
  const [subjectIn, setSubjectIn] = useState("");
  const [featureIn, setFeatureIn] = useState("");
  const [minIn, setMinIn] = useState("");
  const [f, setF] = useState({ subject: "", feature: "", min: "" });
  const [offset, setOffset] = useState(0);
  const meters = useLoad(
    (s) => listUsageMeters({ subject: f.subject || undefined, feature: f.feature || undefined, min_percent: f.min ? Number(f.min) : undefined, limit: PAGE, offset }, s),
    `meters:${f.subject}:${f.feature}:${f.min}:${offset}`,
  );
  const alerts = useLoad((s) => (f.subject ? listUsageAlerts(f.subject, s) : Promise.resolve(undefined)), `alerts:${f.subject}`);
  return (
    <div className="space-y-4">
      <form className="flex flex-wrap items-end gap-2 text-sm" onSubmit={(e) => { e.preventDefault(); setOffset(0); setF({ subject: subjectIn.trim(), feature: featureIn.trim(), min: minIn.trim() }); }}>
        <label>Usage subject<input className={`${inputCls} ml-2 w-48`} value={subjectIn} onChange={(e) => setSubjectIn(e.target.value)} placeholder="all subjects" /></label>
        <label>Feature<input className={`${inputCls} ml-2 w-36`} value={featureIn} onChange={(e) => setFeatureIn(e.target.value)} placeholder="all features" /></label>
        <label>Min % used<input className={`${inputCls} ml-2 w-20`} inputMode="numeric" value={minIn} onChange={(e) => setMinIn(e.target.value)} /></label>
        <Button type="submit">Show usage</Button>
      </form>
      <BillingError error={meters.error} />
      {meters.loading && !meters.error && <Loading what="Loading usage" />}
      {meters.data && meters.data.items.length === 0 && <Empty>No usage meters match. Meters appear once a subject uses a metered feature.</Empty>}
      {meters.data && meters.data.items.length > 0 && <ul aria-label="Usage meters" className="grid gap-3 md:grid-cols-2">{meters.data.items.map((u) => <Meter key={`${u.subject}:${u.feature}:${u.scope_key}`} u={u} />)}</ul>}
      {meters.data && meters.data.total > PAGE && (
        <div className="flex items-center gap-3 text-sm text-zinc-400" aria-label="Usage paging">
          <span>{meters.data.offset + 1}-{meters.data.offset + meters.data.items.length} of {meters.data.total}</span>
          <Button disabled={offset === 0} onClick={() => setOffset(Math.max(0, offset - PAGE))}>Previous</Button>
          <Button disabled={offset + PAGE >= meters.data.total} onClick={() => setOffset(offset + PAGE)}>Next</Button>
        </div>
      )}
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
