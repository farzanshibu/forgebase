import { useState } from "react";
import { EmptyState } from "../../components/EmptyState";
import { ENTRY_KINDS, creditAudit, creditBalance, creditConsumption, creditGrant, creditLedger, creditLots, creditSummary, creditTopUp, listCreditRates } from "../../api";
import type { EntryKind } from "../../api";
import { Button, ErrorView } from "../../components/ui";
import { Sel } from "../../components/controls";
import { SkeletonCards, SkeletonTable } from "../../components/Skeleton";
import { useLoad } from "../../hooks";
import { formatCount } from "./money";
import { BillingError, Field, Filter, None, Note, TableCard } from "./shared";
import { idempotencyKey, words } from "./util";
import { useAct } from "./useAct";
import { Table } from "panelui-web/table";
import { Input } from "panelui-web/input";
import { Badge as PanelBadge } from "panelui-web/badge";
import { Stat, StatGrid } from "../../components/Stats";
import { Th } from "../../components/TableCard";
import { When } from "../../components/Time";

const n = formatCount;

function Platform() {
  const summary = useLoad((s) => creditSummary({ all: true }, s), "credit-summary");
  const consumption = useLoad((s) => creditConsumption({ all: true }, s), "credit-consumption");
  const rates = useLoad(listCreditRates, "credit-rates");
  const d = summary.data;
  return (
    <div className="space-y-5">
      <BillingError error={summary.error} />
      {summary.loading && !summary.error && <SkeletonCards count={5} label="Loading credit statistics" />}
      {d && (
        <section aria-labelledby="credit-30d" className="space-y-2">
          <h2 id="credit-30d" className="text-sm font-semibold">Last 30 days, whole instance</h2>
          <StatGrid label="Credit summary" columns={5}>
            {([["Top-ups", d.top_up], ["Grants", d.grant], ["Consumed", d.consumption], ["Spent", d.spend], ["Refunded", d.refund], ["Reversed", d.reversal], ["Expired", d.expiry], ["Net", d.net], ["Balance", d.balance], ["Shortfall", d.shortfall]] as const).map(([k, v]) => (
              <Stat key={k} label={k} value={n(v)} tone={k === "Shortfall" && v > 0 ? "bad" : undefined} />
            ))}
          </StatGrid>
        </section>
      )}
      <div className="grid gap-5 xl:grid-cols-[2fr_1fr]">
        <TableCard title="Consumption by feature" description="Credits debited for metered use, per period.">
          {consumption.loading && !consumption.error && <SkeletonTable rows={3} columns={6} label="Loading consumption" />}
          <ErrorView error={consumption.error} />
          {consumption.data && consumption.data.rows.length === 0 && <EmptyState compact title="No consumption in this window" why="Credits used by metered features appear here." />}
          {consumption.data && consumption.data.rows.length > 0 && (
            <Table framed aria-label="Credit consumption">
              <Table.Header><Table.Row><Th>Period</Th><Th>Feature</Th><Th align="end">Units</Th><Th align="end">Debited</Th><Th align="end">Refunded</Th><Th align="end">Net</Th></Table.Row></Table.Header>
              <Table.Body>
                {consumption.data.rows.map((r) => (
                  <Table.Row key={`${r.start}:${r.feature}`}>
                    <Table.Cell className="text-muted-foreground"><When iso={r.start} day /></Table.Cell>
                    <Table.Cell className="font-mono text-xs">{r.feature}</Table.Cell>
                    <Table.Cell align="end">{n(r.units)}</Table.Cell>
                    <Table.Cell align="end">{n(r.debited)}</Table.Cell>
                    <Table.Cell align="end">{n(r.refunded)}</Table.Cell>
                    <Table.Cell align="end" className="font-medium">{n(r.net)}</Table.Cell>
                  </Table.Row>
                ))}
              </Table.Body>
              <Table.Footer><Table.Row><Table.Cell colSpan={5}>Total net</Table.Cell><Table.Cell align="end">{n(consumption.data.total_net)}</Table.Cell></Table.Row></Table.Footer>
            </Table>
          )}
        </TableCard>
        <TableCard title="Credit prices" description="What one use of a metered feature costs in credits.">
          {rates.data && rates.data.length === 0 && <EmptyState compact title="No feature is priced in credits" why="Give a metered feature a credit price in Entitlements to bill it from credits." />}
          {rates.data && rates.data.length > 0 && (
            <ul aria-label="Credit rates" className="divide-y divide-border text-sm">
              {rates.data.map((r) => (
                <li key={r.feature} className="flex items-baseline justify-between gap-3 px-4 py-2.5">
                  <span className="font-mono text-xs">{r.feature}</span>
                  <span className="text-right tabular-nums">{n(r.credits_per_unit)} credits per {n(r.per_units)} units <span className="text-xs text-muted-foreground">({r.basis})</span></span>
                </li>
              ))}
            </ul>
          )}
        </TableCard>
      </div>
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
  const amountOk = /^[1-9]\d*$/.test(amount.trim());
  const post = (which: "top" | "grant") => async () => {
    const amt = Number(amount);
    if (!Number.isInteger(amt) || amt <= 0) return;
    const input = { subject, amount: amt, reason: reason || null, idempotency_key: idempotencyKey("cr") };
    await act.run(() => (which === "top" ? creditTopUp(input) : creditGrant(input)), which === "top" ? "Top-up posted" : "Grant posted");
  };
  return (
    <div className="space-y-5">
      <BillingError error={balance.error} />
      {balance.data && (
        <div className="grid gap-4 lg:grid-cols-[1fr_2fr]">
          <section aria-label="Credit balance" className="rounded-xl border border-border bg-card p-4">
            <div className="flex items-center justify-between gap-2">
              <span className="text-xs font-medium text-muted-foreground">Balance</span>
              {audit.data && <PanelBadge size="sm" dot variant={audit.data.consistent ? "success" : "destructive"}>{audit.data.consistent ? "ledger consistent" : "ledger mismatch"}</PanelBadge>}
            </div>
            <p className="mt-1 text-3xl font-semibold tracking-tight tabular-nums">{n(balance.data.balance)} <span className="text-base font-normal text-muted-foreground">credits</span></p>
            {balance.data.expiring.length > 0 && <p className="mt-1 text-xs text-muted-foreground">Next expiry: {n(balance.data.expiring[0]!.amount)} on <When iso={balance.data.expiring[0]!.expires_at} day /></p>}
            {lots.data && lots.data.length > 0 && (
              <ul aria-label="Credit lots" className="mt-3 space-y-1 border-t border-border pt-3 text-xs text-muted-foreground">
                {lots.data.map((l) => <li key={l.id} className="flex justify-between gap-2"><span>{words(l.kind)}</span><span className="tabular-nums">{n(l.remaining)} of {n(l.amount)} left{l.expires_at ? <>, expires <When iso={l.expires_at} day /></> : ""}</span></li>)}
              </ul>
            )}
          </section>
          <section aria-labelledby="credit-adjust" className="rounded-xl border border-border bg-card p-4">
            <h3 id="credit-adjust" className="text-sm font-semibold">Add credits</h3>
            <p className="mt-0.5 text-xs text-muted-foreground">Purchased credits count as revenue; granted credits are free (support, promotions).</p>
            <div className="mt-3 flex flex-wrap items-end gap-3">
              <Field label="Credits" className="w-32"><Input size="sm" className="tabular-nums" inputMode="numeric" value={amount} onChange={(e) => setAmount(e.target.value)} placeholder="250" /></Field>
              <Field label="Reason" className="min-w-48 flex-1"><Input size="sm" value={reason} onChange={(e) => setReason(e.target.value)} placeholder="Optional" /></Field>
              <Button disabled={act.busy || !amountOk} onClick={post("grant")}>Grant free credits</Button>
              <Button tone="primary" disabled={act.busy || !amountOk} onClick={post("top")}>Add purchased credits</Button>
            </div>
            <ErrorView error={act.error} />
            <Note className="mt-2">{act.note}</Note>
          </section>
        </div>
      )}
      <TableCard
        title="Ledger"
        count={ledger.data?.items.length}
        description="Every grant, purchase and use, newest first."
        toolbar={<Filter label="Entry kind">{(id) => <Sel placeholder="All kinds" id={id} value={kind} onChange={(v) => setKind(v as EntryKind | "")} options={[{ value: "", label: "All kinds" }, ...ENTRY_KINDS.map((k) => ({ value: k, label: words(k) }))]} className="w-40" />}</Filter>}
      >
        {ledger.loading && !ledger.error && <SkeletonTable rows={3} columns={6} label="Loading ledger" />}
        {ledger.data && ledger.data.items.length === 0 && <EmptyState compact title="No ledger entries" why="Every credit grant, purchase and use is listed here." />}
        {ledger.data && ledger.data.items.length > 0 && (
          <Table framed aria-label="Credit ledger">
            <Table.Header><Table.Row><Th align="end">#</Th><Th>When</Th><Th>Kind</Th><Th align="end">Amount</Th><Th align="end">Balance after</Th><Th>Feature</Th><Th>Reason</Th></Table.Row></Table.Header>
            <Table.Body>
              {ledger.data.items.map((e) => (
                <Table.Row key={e.id}>
                  <Table.Cell align="end" className="text-muted-foreground">{e.seq}</Table.Cell>
                  <Table.Cell className="text-muted-foreground"><When iso={e.at} /></Table.Cell>
                  <Table.Cell>{words(e.kind)}</Table.Cell>
                  <Table.Cell align="end" className={`font-medium ${e.amount < 0 ? "text-destructive" : "text-success"}`}>{e.amount > 0 ? "+" : ""}{n(e.amount)}</Table.Cell>
                  <Table.Cell align="end">{n(e.balance_after)}</Table.Cell>
                  <Table.Cell className="font-mono text-xs">{e.feature ?? <None />}</Table.Cell>
                  <Table.Cell>{e.reason ?? <None />}</Table.Cell>
                </Table.Row>
              ))}
            </Table.Body>
          </Table>
        )}
      </TableCard>
    </div>
  );
}

export default function Credits() {
  const [input, setInput] = useState("");
  const [subject, setSubject] = useState("");
  return (
    <div className="space-y-6">
      <Platform />
      <section aria-labelledby="credit-ledger" className="space-y-4 border-t border-border pt-5">
        <div className="flex flex-wrap items-end justify-between gap-3">
          <div>
            <h2 id="credit-ledger" className="text-sm font-semibold">Customer ledger</h2>
            <p className="mt-0.5 text-xs text-muted-foreground">Open one customer's credits to see the balance, every entry, and to add credits.</p>
          </div>
          <form className="flex items-center gap-2" onSubmit={(e) => { e.preventDefault(); setSubject(input.trim()); }}>
            <Filter label="Subject">{(id) => <Input id={id} size="sm" className="w-56 font-mono" value={input} onChange={(e) => setInput(e.target.value)} placeholder="org:acme or a user key" />}</Filter>
            <Button type="submit" disabled={!input.trim()}>Open ledger</Button>
          </form>
        </div>
        {subject ? <Ledger key={subject} subject={subject} /> : <EmptyState compact title="No customer chosen" why="Type a subject such as org:acme and open its ledger." />}
      </section>
    </div>
  );
}
