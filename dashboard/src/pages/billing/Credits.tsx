import { useState } from "react";
import { ENTRY_KINDS, creditAudit, creditBalance, creditConsumption, creditGrant, creditLedger, creditLots, creditSummary, creditTopUp, listCreditRates } from "../../api";
import type { EntryKind } from "../../api";
import { Button, ErrorView, Loading, Panel, inputCls } from "../../components/ui";
import { useLoad } from "../../hooks";
import { formatCount } from "./money";
import { BillingError, Empty, Field, Th } from "./shared";
import { ROW, TABLE, THEAD, idempotencyKey, when_ } from "./util";
import { useAct } from "./useAct";

const n = formatCount;

function Platform() {
  const summary = useLoad((s) => creditSummary({ all: true }, s), "credit-summary");
  const consumption = useLoad((s) => creditConsumption({ all: true }, s), "credit-consumption");
  const rates = useLoad(listCreditRates, "credit-rates");
  const d = summary.data;
  return (
    <div className="space-y-4">
      <BillingError error={summary.error} />
      {summary.loading && !summary.error && <Loading what="Loading credit statistics" />}
      {d && (
        <Panel title="Credits, last 30 days (whole instance)">
          <dl aria-label="Credit summary" className="grid gap-3 text-sm sm:grid-cols-4 lg:grid-cols-6">
            {([["Top-ups", d.top_up], ["Grants", d.grant], ["Consumed", d.consumption], ["Spent", d.spend], ["Refunded", d.refund], ["Reversed", d.reversal], ["Expired", d.expiry], ["Net", d.net], ["Balance", d.balance], ["Shortfall", d.shortfall]] as const).map(([k, v]) => (
              <div key={k}><dt className="text-zinc-500">{k}</dt><dd className="tabular-nums">{n(v)}</dd></div>
            ))}
          </dl>
        </Panel>
      )}
      <Panel title="Credit consumption by feature">
        {consumption.data && consumption.data.rows.length === 0 && <Empty>No consumption in this window.</Empty>}
        {consumption.data && consumption.data.rows.length > 0 && (
          <table aria-label="Credit consumption" className={TABLE}>
            <thead className={THEAD}><tr><Th>Period</Th><Th>Feature</Th><Th>Units</Th><Th>Debited</Th><Th>Refunded</Th><Th>Net</Th></tr></thead>
            <tbody>{consumption.data.rows.map((r) => <tr key={`${r.start}:${r.feature}`} className={ROW}><td className="py-1.5 pr-3 text-zinc-400">{when_(r.start)}</td><td>{r.feature}</td><td>{n(r.units)}</td><td>{n(r.debited)}</td><td>{n(r.refunded)}</td><td>{n(r.net)}</td></tr>)}</tbody>
            <tfoot><tr className={ROW}><td className="py-1.5 font-medium" colSpan={5}>Total net</td><td className="font-medium">{n(consumption.data.total_net)}</td></tr></tfoot>
          </table>
        )}
      </Panel>
      <Panel title="Credit prices of metered features">
        {rates.data && rates.data.length === 0 && <Empty>No feature is priced in credits.</Empty>}
        {rates.data && rates.data.length > 0 && <ul aria-label="Credit rates" className="text-sm">{rates.data.map((r) => <li key={r.feature}>{r.feature}: {n(r.credits_per_unit)} credits per {n(r.per_units)} units ({r.basis})</li>)}</ul>}
      </Panel>
    </div>
  );
}

function Ledger({ subject }: { subject: string }) {
  const [kind, setKind] = useState<EntryKind | "">("");
  const balance = useLoad((s) => creditBalance(subject, s), `bal:${subject}`);
  const lots = useLoad((s) => creditLots(subject, false, s), `lots:${subject}`);
  const ledger = useLoad((s) => creditLedger({ subject, kind: kind || undefined, limit: 100 }, s), `ledger:${subject}:${kind}`);
  const audit = useLoad((s) => creditAudit(subject, s), `audit:${subject}`);
  const reload = () => { balance.reload(); lots.reload(); ledger.reload(); audit.reload(); };
  const act = useAct(reload);
  const [amount, setAmount] = useState("");
  const [reason, setReason] = useState("");
  const post = (which: "top" | "grant") => async () => {
    const amt = Number(amount);
    if (!Number.isInteger(amt) || amt <= 0) return;
    const input = { subject, amount: amt, reason: reason || null, idempotency_key: idempotencyKey("cr") };
    await act.run(() => (which === "top" ? creditTopUp(input) : creditGrant(input)), which === "top" ? "Top-up posted" : "Grant posted");
  };
  return (
    <Panel title={`Ledger of ${subject}`}>
      <BillingError error={balance.error} />
      {balance.data && (
        <p aria-label="Credit balance" className="text-sm">Balance <span className="text-xl font-semibold tabular-nums">{n(balance.data.balance)}</span> credits
          {balance.data.expiring.length > 0 && <span className="ml-2 text-zinc-400">next expiry {n(balance.data.expiring[0]!.amount)} on {when_(balance.data.expiring[0]!.expires_at)}</span>}
          {audit.data && <span className={`ml-3 text-xs ${audit.data.consistent ? "text-emerald-300" : "text-red-300"}`}>{audit.data.consistent ? "ledger consistent" : "ledger mismatch"}</span>}
        </p>
      )}
      <div className="mt-3 flex flex-wrap items-end gap-2 text-sm">
        <Field label="Credits"><input className={`${inputCls} w-32`} inputMode="numeric" value={amount} onChange={(e) => setAmount(e.target.value)} /></Field>
        <Field label="Reason"><input className={`${inputCls} w-48`} value={reason} onChange={(e) => setReason(e.target.value)} /></Field>
        <Button disabled={act.busy} onClick={post("top")}>Add purchased credits</Button>
        <Button disabled={act.busy} onClick={post("grant")}>Grant free credits</Button>
      </div>
      <ErrorView error={act.error} />
      {act.note && <p role="status" className="mt-2 text-sm text-emerald-300">{act.note}</p>}
      {lots.data && lots.data.length > 0 && (
        <ul aria-label="Credit lots" className="mt-3 text-sm text-zinc-300">
          {lots.data.map((l) => <li key={l.id}>{l.kind}: {n(l.remaining)} of {n(l.amount)} left{l.expires_at ? `, expires ${when_(l.expires_at)}` : ""}</li>)}
        </ul>
      )}
      <div className="mt-4 text-sm">
        <label>Entry kind<select className={`${inputCls} ml-2`} value={kind} onChange={(e) => setKind(e.target.value as EntryKind | "")}><option value="">all</option>{ENTRY_KINDS.map((k) => <option key={k}>{k}</option>)}</select></label>
      </div>
      {ledger.data && ledger.data.items.length === 0 && <Empty>No ledger entries.</Empty>}
      {ledger.data && ledger.data.items.length > 0 && (
        <table aria-label="Credit ledger" className={`mt-2 ${TABLE}`}>
          <thead className={THEAD}><tr><Th>#</Th><Th>When</Th><Th>Kind</Th><Th>Amount</Th><Th>Balance after</Th><Th>Feature</Th><Th>Reason</Th></tr></thead>
          <tbody>{ledger.data.items.map((e) => <tr key={e.id} className={ROW}><td className="py-1.5 pr-3">{e.seq}</td><td className="text-zinc-400">{when_(e.at)}</td><td>{e.kind}</td><td className={e.amount < 0 ? "text-red-300" : "text-emerald-300"}>{e.amount > 0 ? "+" : ""}{n(e.amount)}</td><td>{n(e.balance_after)}</td><td>{e.feature ?? "-"}</td><td>{e.reason ?? "-"}</td></tr>)}</tbody>
        </table>
      )}
    </Panel>
  );
}

export default function Credits() {
  const [input, setInput] = useState("");
  const [subject, setSubject] = useState("");
  return (
    <div className="space-y-4">
      <Platform />
      <form className="flex items-end gap-2 text-sm" onSubmit={(e) => { e.preventDefault(); setSubject(input.trim()); }}>
        <label>Ledger subject<input className={`${inputCls} ml-2 w-56`} value={input} onChange={(e) => setInput(e.target.value)} placeholder="org:acme or a user key" /></label>
        <Button type="submit">Open ledger</Button>
      </form>
      {subject && <Ledger key={subject} subject={subject} />}
    </div>
  );
}
