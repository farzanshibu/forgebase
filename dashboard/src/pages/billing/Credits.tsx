import { useState } from "react";
import { EmptyState } from "../../components/EmptyState";
import { ENTRY_KINDS, creditAudit, creditBalance, creditConsumption, creditGrant, creditLedger, creditLots, creditSummary, creditTopUp, listCreditRates } from "../../api";
import type { EntryKind } from "../../api";
import { Button, ErrorView, Loading, Panel } from "../../components/ui";
import { Sel } from "../../components/controls";
import { useLoad } from "../../hooks";
import { formatCount } from "./money";
import { BillingError, Field, Th } from "./shared";
import { ROW, TABLE, THEAD, idempotencyKey, when_ } from "./util";
import { useAct } from "./useAct";
import { Table } from "panelui-web/table";
import { Input } from "panelui-web/input";

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
              <div key={k}><dt className="text-muted-foreground">{k}</dt><dd className="tabular-nums">{n(v)}</dd></div>
            ))}
          </dl>
        </Panel>
      )}
      <Panel title="Credit consumption by feature">
        {consumption.data && consumption.data.rows.length === 0 && <EmptyState compact title="No consumption in this window." why="Pick a longer window to see earlier use." />}
        {consumption.data && consumption.data.rows.length > 0 && (
          <Table aria-label="Credit consumption" className={TABLE}>
            <Table.Header className={THEAD}><Table.Row><Th>Period</Th><Th>Feature</Th><Th>Units</Th><Th>Debited</Th><Th>Refunded</Th><Th>Net</Th></Table.Row></Table.Header>
            <Table.Body>{consumption.data.rows.map((r) => <Table.Row key={`${r.start}:${r.feature}`} className={ROW}><Table.Cell className="py-1.5 pr-3 text-muted-foreground">{when_(r.start)}</Table.Cell><Table.Cell>{r.feature}</Table.Cell><Table.Cell>{n(r.units)}</Table.Cell><Table.Cell>{n(r.debited)}</Table.Cell><Table.Cell>{n(r.refunded)}</Table.Cell><Table.Cell>{n(r.net)}</Table.Cell></Table.Row>)}</Table.Body>
            <Table.Footer><Table.Row className={ROW}><Table.Cell className="py-1.5 font-medium" colSpan={5}>Total net</Table.Cell><Table.Cell className="font-medium">{n(consumption.data.total_net)}</Table.Cell></Table.Row></Table.Footer>
          </Table>
        )}
      </Panel>
      <Panel title="Credit prices of metered features">
        {rates.data && rates.data.length === 0 && <EmptyState compact title="No feature is priced in credits." why="Give a feature a credit price in the catalog to meter it." />}
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
          {balance.data.expiring.length > 0 && <span className="ml-2 text-muted-foreground">next expiry {n(balance.data.expiring[0]!.amount)} on {when_(balance.data.expiring[0]!.expires_at)}</span>}
          {audit.data && <span className={`ml-3 text-xs ${audit.data.consistent ? "text-success" : "text-destructive"}`}>{audit.data.consistent ? "ledger consistent" : "ledger mismatch"}</span>}
        </p>
      )}
      <div className="mt-3 flex flex-wrap items-end gap-2 text-sm">
        <Field label="Credits"><Input size="sm" className="w-32" inputMode="numeric" value={amount} onChange={(e) => setAmount(e.target.value)} /></Field>
        <Field label="Reason"><Input size="sm" className="w-48" value={reason} onChange={(e) => setReason(e.target.value)} /></Field>
        <Button disabled={act.busy} onClick={post("top")}>Add purchased credits</Button>
        <Button disabled={act.busy} onClick={post("grant")}>Grant free credits</Button>
      </div>
      <ErrorView error={act.error} />
      {act.note && <p role="status" className="mt-2 text-sm text-success">{act.note}</p>}
      {lots.data && lots.data.length > 0 && (
        <ul aria-label="Credit lots" className="mt-3 text-sm text-foreground/80">
          {lots.data.map((l) => <li key={l.id}>{l.kind}: {n(l.remaining)} of {n(l.amount)} left{l.expires_at ? `, expires ${when_(l.expires_at)}` : ""}</li>)}
        </ul>
      )}
      <div className="mt-4 text-sm">
        <label>Entry kind<Sel value={kind} onChange={(v) => setKind(v as EntryKind | "")} options={[{ value: "", label: "all" }, ...ENTRY_KINDS]} className="ml-2" /></label>
      </div>
      {ledger.data && ledger.data.items.length === 0 && <EmptyState compact title="No ledger entries." why="Every credit grant and use is listed here." />}
      {ledger.data && ledger.data.items.length > 0 && (
        <Table aria-label="Credit ledger" className={`mt-2 ${TABLE}`}>
          <Table.Header className={THEAD}><Table.Row><Th>#</Th><Th>When</Th><Th>Kind</Th><Th>Amount</Th><Th>Balance after</Th><Th>Feature</Th><Th>Reason</Th></Table.Row></Table.Header>
          <Table.Body>{ledger.data.items.map((e) => <Table.Row key={e.id} className={ROW}><Table.Cell className="py-1.5 pr-3">{e.seq}</Table.Cell><Table.Cell className="text-muted-foreground">{when_(e.at)}</Table.Cell><Table.Cell>{e.kind}</Table.Cell><Table.Cell className={e.amount < 0 ? "text-destructive" : "text-success"}>{e.amount > 0 ? "+" : ""}{n(e.amount)}</Table.Cell><Table.Cell>{n(e.balance_after)}</Table.Cell><Table.Cell>{e.feature ?? "-"}</Table.Cell><Table.Cell>{e.reason ?? "-"}</Table.Cell></Table.Row>)}</Table.Body>
        </Table>
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
        <label>Ledger subject<Input size="sm" className="ml-2 w-56" value={input} onChange={(e) => setInput(e.target.value)} placeholder="org:acme or a user key" /></label>
        <Button type="submit">Open ledger</Button>
      </form>
      {subject && <Ledger key={subject} subject={subject} />}
    </div>
  );
}
